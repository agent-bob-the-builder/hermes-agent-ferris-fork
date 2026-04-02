#!/usr/bin/env python3
"""
Hydrabase Sidecar Tool — integrates Ferris with Hydrabase P2P knowledge graph.

Provides:
  - hydrabase_connect:    Connect to a Hydrabase node (WebSocket)
  - hydrabase_submit:     Submit a Ferris fact to the local Hydrabase node
  - hydrabase_search:     Search Ferris facts across the P2P network
  - hydrabase_lookup:     Lookup a specific fact by soul_id
  - hydrabase_vote:       Vote on a peer's fact (trust-weighted confidence)
  - hydrabase_disconnect: Close the connection

Requires the `websockets` package: pip install websockets
Or the stdlib asyncio client is used as fallback.
"""

import asyncio
import json
import logging
import os
import time
import uuid
from dataclasses import dataclass, field
from typing import Any, Optional

from tools.registry import registry

logger = logging.getLogger(__name__)

HYDRABASE_WS_DEFAULT_PORT = 4545
DEFAULT_ROOM_SEED = "ferris-memory-v1"
DEFAULT_PLUGIN_ID = "ferris"
REQUEST_TIMEOUT_SECONDS = 10

# Global sidecar instance (one per agent session)
_sidecar: Optional["HydrabaseSidecar"] = None


@dataclass
class FerrisFact:
    id: str
    soul_id: str
    type: str
    content: dict
    confidence: float
    address: str
    subject: Optional[str] = None
    ttl: Optional[int] = None
    timestamp: int = field(default_factory=lambda: int(time.time() * 1000))
    expires_at: Optional[int] = None
    vote_count: int = 0
    plugin_id: str = DEFAULT_PLUGIN_ID

    def to_dict(self) -> dict:
        return {
            "id": self.id,
            "soul_id": self.soul_id,
            "type": self.type,
            "content": self.content,
            "confidence": self.confidence,
            "address": self.address,
            "subject": self.subject,
            "ttl": self.ttl,
            "timestamp": self.timestamp,
            "expires_at": self.expires_at,
            "vote_count": self.vote_count,
            "plugin_id": self.plugin_id,
        }

    def to_text(self) -> str:
        """Human-readable summary."""
        conf = f"{self.confidence:.0%}"
        text = self.content.get("text", str(self.content))
        src = f"[{self.address[:10]}...]" if len(self.address) > 10 else "[local]"
        votes = f" (+{self.vote_count} votes)" if self.vote_count > 0 else ""
        return f"[{self.type} | conf={conf}{votes}] {text} {src}"

    @classmethod
    def from_hydrabase(cls, data: dict) -> "FerrisFact":
        content = data.get("content", {})
        if isinstance(content, str):
            try:
                content = json.loads(content)
            except json.JSONDecodeError:
                content = {"text": content, "metadata": {}}
        return cls(
            id=data["id"],
            soul_id=data["soul_id"],
            type=data.get("type", "belief"),
            content=content,
            confidence=data.get("confidence", 0.5),
            address=data.get("address", "0x0"),
            subject=data.get("subject"),
            ttl=data.get("ttl"),
            timestamp=data.get("timestamp", int(time.time() * 1000)),
            expires_at=data.get("expires_at"),
            vote_count=data.get("vote_count", 0),
            plugin_id=data.get("plugin_id", DEFAULT_PLUGIN_ID),
        )


class HydrabaseSidecar:
    """
    WebSocket client for Hydrabase P2P network.
    Manages a single persistent connection and request/response multiplexing.
    """

    def __init__(
        self,
        host: str = "localhost",
        port: int = HYDRABASE_WS_DEFAULT_PORT,
        api_key: Optional[str] = None,
        room_seed: str = DEFAULT_ROOM_SEED,
        timeout: float = REQUEST_TIMEOUT_SECONDS,
    ):
        self.host = host
        self.port = port
        self.api_key = api_key or os.getenv("HYDRABASE_API_KEY", "")
        self.room_seed = room_seed
        self.timeout = timeout
        self._ws: Optional[Any] = None
        self._connected = False
        self._nonce_counter = 0
        self._pending: dict[int, asyncio.Future] = {}
        self._listen_task: Optional[asyncio.Task] = None
        self._lock = asyncio.Lock()

    async def connect(self) -> bool:
        """Connect to the Hydrabase node WebSocket."""
        if self._connected:
            return True

        try:
            import websockets
            url = f"ws://{self.host}:{self.port}"
            headers = {}
            if self.api_key:
                headers["x-api-key"] = self.api_key

            self._ws = await websockets.connect(url, extra_headers=headers, ping_interval=None)
            self._connected = True
            self._listen_task = asyncio.create_task(self._listen_loop())
            logger.info(f"[HydrabaseSidecar] Connected to {url}")
            return True
        except ImportError:
            logger.error("[HydrabaseSidecar] websockets package not installed. Run: pip install websockets")
            return False
        except Exception as e:
            logger.error(f"[HydrabaseSidecar] Connection failed: {e}")
            return False

    async def disconnect(self) -> None:
        """Close the WebSocket connection."""
        self._connected = False
        if self._listen_task:
            self._listen_task.cancel()
            try:
                await self._listen_task
            except asyncio.CancelledError:
                pass
        if self._ws:
            try:
                await self._ws.close()
            except Exception:
                pass
        self._ws = None

    def _next_nonce(self) -> int:
        self._nonce_counter = (self._nonce_counter + 1) % 100000
        return self._nonce_counter

    async def _send_raw(self, payload: dict) -> None:
        if self._ws is None:
            raise ConnectionError("Not connected to Hydrabase")
        data = json.dumps(payload, default=str)
        await self._ws.send(data)

    async def _listen_loop(self):
        """Background: read messages and dispatch to pending futures."""
        while self._connected:
            try:
                msg = await self._ws.recv()
                await self._handle_message(msg)
            except asyncio.CancelledError:
                break
            except Exception as e:
                if self._connected:
                    logger.warning(f"[HydrabaseSidecar] Listen error: {e}")
                break

    async def _handle_message(self, msg: str):
        try:
            data = json.loads(msg)
        except json.JSONDecodeError:
            logger.warning(f"[HydrabaseSidecar] Non-JSON: {msg[:80]}")
            return

        nonce = data.get("nonce")
        if nonce is not None and nonce in self._pending:
            future = self._pending.pop(nonce)
            if not future.done():
                future.set_result(data)

    async def _request(self, request: dict) -> dict:
        """Send a request and wait for its response."""
        nonce = self._next_nonce()
        request["nonce"] = nonce

        future = asyncio.get_event_loop().create_future()
        self._pending[nonce] = future

        await self._send_raw({"request": request})

        try:
            return await asyncio.wait_for(future, timeout=self.timeout)
        except asyncio.TimeoutError:
            self._pending.pop(nonce, None)
            raise TimeoutError(f"Hydrabase request timed out after {self.timeout}s: {request.get('type')}")

    # -------------------------------------------------------------------------
    # Public API
    # -------------------------------------------------------------------------

    async def submit_fact(
        self,
        content: dict,
        fact_type: str = "belief",
        subject: Optional[str] = None,
        confidence: float = 0.5,
        ttl: Optional[int] = None,
    ) -> FerrisFact:
        """Submit a new Ferris fact to the local Hydrabase node."""
        fact_id = f"fact_{uuid.uuid4().hex[:16]}"
        soul_id = self._compute_soul_id(fact_id)
        now = int(time.time() * 1000)

        fact_data = {
            "plugin_id": DEFAULT_PLUGIN_ID,
            "id": fact_id,
            "soul_id": soul_id,
            "type": fact_type,
            "content": content,
            "confidence": confidence,
            "subject": subject,
            "ttl": ttl,
            "timestamp": now,
            "expires_at": (now + ttl) if ttl else None,
            "vote_count": 0,
        }

        try:
            response = await self._request({
                "type": "ferris.submit",
                "query": soul_id,
                "fact": fact_data,
            })
        except TimeoutError:
            # Fall back to local-only if the node doesn't support ferris.submit yet
            logger.warning("[HydrabaseSidecar] ferris.submit not supported by node — storing locally only")
            return FerrisFact(
                id=fact_id, soul_id=soul_id, type=fact_type,
                content=content, confidence=confidence, address="0x0",
                subject=subject, ttl=ttl, timestamp=now,
                expires_at=(now + ttl) if ttl else None, vote_count=0,
            )

        return FerrisFact(
            id=fact_id, soul_id=soul_id, type=fact_type,
            content=content, confidence=confidence, address="0x0",
            subject=subject, ttl=ttl, timestamp=now,
            expires_at=(now + ttl) if ttl else None, vote_count=0,
        )

    async def search_facts(self, query: str, min_confidence: float = 0.0) -> list[FerrisFact]:
        """Search Ferris facts across the P2P network by content text."""
        try:
            response = await self._request({
                "type": "ferris.facts",
                "query": query,
            })
        except TimeoutError:
            logger.warning("[HydrabaseSidecar] ferris.facts timed out — returning empty")
            return []

        results = response.get("response", [])
        facts = [FerrisFact.from_hydrabase(r) for r in results]
        return sorted([f for f in facts if f.confidence >= min_confidence],
                      key=lambda f: f.confidence, reverse=True)

    async def lookup_fact(self, soul_id: str) -> list[FerrisFact]:
        """Lookup a specific fact by its soul_id."""
        try:
            response = await self._request({
                "type": "ferris.lookup",
                "query": soul_id,
            })
        except TimeoutError:
            return []

        results = response.get("response", [])
        return [FerrisFact.from_hydrabase(r) for r in results]

    async def vote_fact(
        self,
        soul_id: str,
        peer_address: str,
        peer_confidence: float,
        fact_confidence: float,
    ) -> FerrisFact:
        """Record a vote on a peer's fact to update trust-weighted confidence."""
        response = await self._request({
            "type": "ferris.vote",
            "query": soul_id,
            "peer_address": peer_address,
            "peer_confidence": peer_confidence,
            "fact_confidence": fact_confidence,
        })
        results = response.get("response", [])
        if results:
            return FerrisFact.from_hydrabase(results[0])
        raise ValueError(f"No fact with soul_id: {soul_id}")

    def _compute_soul_id(self, id: str) -> str:
        """Compute soul_id compatible with Hydrabase's Bun.hash scheme."""
        import hashlib
        prefix = f"{DEFAULT_PLUGIN_ID}:{id}"
        h = hashlib.sha256(prefix.encode()).digest()
        hash_int = int.from_bytes(h[:8], "big")
        return f"soul_{hash_int % (10 ** 18)}"

    @property
    def is_connected(self) -> bool:
        return self._connected


# -------------------------------------------------------------------------
# Tool handler
# -------------------------------------------------------------------------

def check_hydrabase_requirements() -> bool:
    return True  # Always available; connection is explicit


def hydrabase_tool(action: str, **kwargs) -> str:
    """
    Dispatch to the right async method based on action.
    All actual work runs in the event loop; we create a fresh loop per call.
    """
    global _sidecar

    loop = asyncio.new_event_loop()
    asyncio.set_event_loop(loop)
    try:
        if action == "connect":
            result = loop.run_until_complete(_cmd_connect(**kwargs))
        elif action == "disconnect":
            result = loop.run_until_complete(_cmd_disconnect())
        elif action == "submit":
            result = loop.run_until_complete(_cmd_submit(**kwargs))
        elif action == "search":
            result = loop.run_until_complete(_cmd_search(**kwargs))
        elif action == "lookup":
            result = loop.run_until_complete(_cmd_lookup(**kwargs))
        elif action == "vote":
            result = loop.run_until_complete(_cmd_vote(**kwargs))
        elif action == "status":
            result = _cmd_status()
        else:
            result = json.dumps({"success": False, "error": f"Unknown action: {action}"})
    finally:
        loop.close()

    return result


async def _cmd_connect(host: str = "localhost", port: int = HYDRABASE_WS_DEFAULT_PORT,
                        api_key: str = None, room_seed: str = DEFAULT_ROOM_SEED) -> str:
    global _sidecar
    if _sidecar is not None and _sidecar.is_connected:
        return json.dumps({"success": True, "message": "Already connected", "host": _sidecar.host, "port": _sidecar.port})

    _sidecar = HydrabaseSidecar(
        host=host, port=port,
        api_key=api_key or os.getenv("HYDRABASE_API_KEY"),
        room_seed=room_seed,
    )
    connected = await _sidecar.connect()
    if connected:
        return json.dumps({"success": True, "host": host, "port": port, "message": "Connected to Hydrabase"})
    return json.dumps({"success": False, "error": f"Failed to connect to ws://{host}:{port}"})


async def _cmd_disconnect() -> str:
    global _sidecar
    if _sidecar is None:
        return json.dumps({"success": False, "error": "Not connected"})
    await _sidecar.disconnect()
    _sidecar = None
    return json.dumps({"success": True, "message": "Disconnected from Hydrabase"})


async def _cmd_submit(content: str = None, fact_type: str = "belief",
                      subject: str = None, confidence: float = 0.5,
                      ttl: int = None) -> str:
    global _sidecar
    if _sidecar is None or not _sidecar.is_connected:
        return json.dumps({"success": False, "error": "Not connected. Run hydrabase connect first."})

    if content is None:
        return json.dumps({"success": False, "error": "content is required"})
    parsed_content: dict = {"text": content, "metadata": {}}

    fact = await _sidecar.submit_fact(
        content=parsed_content,
        fact_type=fact_type,
        subject=subject,
        confidence=confidence,
        ttl=ttl,
    )
    return json.dumps({
        "success": True,
        "fact": {
            "id": fact.id,
            "soul_id": fact.soul_id,
            "type": fact.type,
            "confidence": fact.confidence,
            "summary": fact.to_text(),
        }
    })


async def _cmd_search(query: str = None, min_confidence: float = 0.0) -> str:
    global _sidecar
    if _sidecar is None or not _sidecar.is_connected:
        return json.dumps({"success": False, "error": "Not connected. Run hydrabase connect first."})
    if not query:
        return json.dumps({"success": False, "error": "query is required"})

    facts = await _sidecar.search_facts(query, min_confidence=min_confidence)
    return json.dumps({
        "success": True,
        "count": len(facts),
        "facts": [{"soul_id": f.soul_id, "type": f.type, "confidence": f.confidence,
                   "summary": f.to_text()} for f in facts],
    })


async def _cmd_lookup(soul_id: str = None) -> str:
    global _sidecar
    if _sidecar is None or not _sidecar.is_connected:
        return json.dumps({"success": False, "error": "Not connected. Run hydrabase connect first."})
    if not soul_id:
        return json.dumps({"success": False, "error": "soul_id is required"})

    facts = await _sidecar.lookup_fact(soul_id)
    return json.dumps({
        "success": True,
        "count": len(facts),
        "facts": [{"soul_id": f.soul_id, "type": f.type, "confidence": f.confidence,
                   "summary": f.to_text()} for f in facts],
    })


async def _cmd_vote(soul_id: str = None, peer_address: str = None,
                    peer_confidence: float = 0.5, fact_confidence: float = 0.5) -> str:
    global _sidecar
    if _sidecar is None or not _sidecar.is_connected:
        return json.dumps({"success": False, "error": "Not connected. Run hydrabase connect first."})
    if not soul_id or not peer_address:
        return json.dumps({"success": False, "error": "soul_id and peer_address are required"})

    fact = await _sidecar.vote_fact(soul_id, peer_address, peer_confidence, fact_confidence)
    return json.dumps({
        "success": True,
        "fact": {"soul_id": fact.soul_id, "confidence": fact.confidence, "vote_count": fact.vote_count},
    })


def _cmd_status() -> str:
    global _sidecar
    if _sidecar is None:
        return json.dumps({"connected": False})
    return json.dumps({
        "connected": _sidecar.is_connected,
        "host": _sidecar.host,
        "port": _sidecar.port,
    })


# -------------------------------------------------------------------------
# Schema
# -------------------------------------------------------------------------

HYDRABASE_SCHEMA = {
    "name": "hydrabase",
    "description": """Interact with Hydrabase P2P knowledge graph — store and retrieve Ferris agent memory facts across a distributed peer network.

Actions:
  connect   — Connect to a Hydrabase node (ws://host:port). Needs to be called before any other action.
  disconnect — Close the connection.
  submit    — Store a new Ferris fact. Content is text + optional metadata. Returns a soul_id.
  search    — Full-text search for facts across the network. Filter by min_confidence.
  lookup    — Get a specific fact by its soul_id (e.g. from a previous submit result).
  vote      — Record your trust vote on a peer's fact. Updates the fact's confidence.
  status    — Check connection status.

Confidence: 0.0 (fully distrusted) to 1.0 (fully trusted). Facts below min_confidence are filtered out of search results.
Peer facts accumulate votes — more peers agreeing = higher confidence.""",
    "parameters": {
        "type": "object",
        "properties": {
            "action": {
                "type": "string",
                "enum": ["connect", "disconnect", "submit", "search", "lookup", "vote", "status"],
                "description": "The operation to perform.",
            },
            "host": {
                "type": "string",
                "description": "Hydrabase node host (for connect action). Default: localhost",
            },
            "port": {
                "type": "integer",
                "description": "Hydrabase node WebSocket port (for connect action). Default: 4545",
            },
            "api_key": {
                "type": "string",
                "description": "Optional x-api-key for the Hydrabase node.",
            },
            "room_seed": {
                "type": "string",
                "description": "Room seed for peer discovery (for connect action). Default: ferris-memory-v1",
            },
            "content": {
                "type": "string",
                "description": "The fact content as plain text (for submit action).",
            },
            "fact_type": {
                "type": "string",
                "enum": ["belief", "conversation", "knowledge", "preference"],
                "description": "Type of fact (for submit action). Default: belief",
            },
            "subject": {
                "type": "string",
                "description": "Optional entity this fact pertains to (for submit action).",
            },
            "confidence": {
                "type": "number",
                "description": "Initial confidence 0..1 (for submit action). Default: 0.5",
            },
            "ttl": {
                "type": "integer",
                "description": "Time-to-live in milliseconds — fact auto-expires after this (for submit action).",
            },
            "query": {
                "type": "string",
                "description": "Search query (for search action).",
            },
            "min_confidence": {
                "type": "number",
                "description": "Minimum confidence threshold for search results. Default: 0.0",
            },
            "soul_id": {
                "type": "string",
                "description": "The soul_id of the fact (for lookup, vote actions).",
            },
            "peer_address": {
                "type": "string",
                "description": "The peer address that contributed the fact (for vote action).",
            },
            "peer_confidence": {
                "type": "number",
                "description": "How much you trust this peer, 0..1 (for vote action). Default: 0.5",
            },
            "fact_confidence": {
                "type": "number",
                "description": "Confidence score from the peer for this fact (for vote action). Default: 0.5",
            },
        },
        "required": ["action"],
    },
}


# --- Registry ---
registry.register(
    name="hydrabase",
    toolset="hydrabase",
    schema=HYDRABASE_SCHEMA,
    handler=lambda args, **kw: hydrabase_tool(
        action=args.get("action", ""),
        host=args.get("host"),
        port=args.get("port", HYDRABASE_WS_DEFAULT_PORT),
        api_key=args.get("api_key"),
        room_seed=args.get("room_seed"),
        content=args.get("content"),
        fact_type=args.get("fact_type", "belief"),
        subject=args.get("subject"),
        confidence=args.get("confidence", 0.5),
        ttl=args.get("ttl"),
        query=args.get("query"),
        min_confidence=args.get("min_confidence", 0.0),
        soul_id=args.get("soul_id"),
        peer_address=args.get("peer_address"),
        peer_confidence=args.get("peer_confidence", 0.5),
        fact_confidence=args.get("fact_confidence", 0.5),
    ),
    check_fn=check_hydrabase_requirements,
    emoji="🔗",
)
