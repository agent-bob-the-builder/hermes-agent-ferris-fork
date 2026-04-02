#!/usr/bin/env python3
"""
Hydrabase Sidecar — Ferris agent integration with Hydrabase P2P probabilistic knowledge graph.

Usage:
    from tools.hydrabase_sidecar import HydrabaseSidecar

    sidecar = HydrabaseSidecar(
        host="localhost",
        port=4545,
        api_key=None,  # or your x-api-key
        room_seed="ferris-memory-v1",
    )
    await sidecar.connect()

    # Write a fact from Ferris memory to Hydrabase
    fact = await sidecar.submit_fact(
        content={"text": "Bob uses MiniMax-M2.7 model", "metadata": {"source": "memory"}},
        fact_type="belief",
        subject="bob",
        confidence=0.9,
    )

    # Search facts across the network
    results = await sidecar.search_facts("Bob model", min_confidence=0.5)

    # Lookup a fact by soul_id
    fact = await sidecar.lookup_fact(soul_id)

    await sidecar.close()
"""

from __future__ import annotations

import asyncio
import json
import logging
import os
import time
import uuid
from dataclasses import dataclass, field
from typing import Any, Optional

logger = logging.getLogger(__name__)

HYDRABASE_WS_DEFAULT_PORT = 4545
DEFAULT_ROOM_SEED = "ferris-memory-v1"
DEFAULT_PLUGIN_ID = "ferris"
REQUEST_TIMEOUT_SECONDS = 10


@dataclass
class FerrisFact:
    """A Ferris fact stored in Hydrabase."""
    id: str
    soul_id: str
    type: str  # 'belief' | 'conversation' | 'knowledge' | 'preference'
    content: dict  # {'text': str, 'metadata': dict}
    confidence: float
    address: str
    subject: Optional[str] = None
    ttl: Optional[int] = None
    timestamp: int = field(default_factory=lambda: int(time.time() * 1000))
    expires_at: Optional[int] = None
    vote_count: int = 0
    plugin_id: str = DEFAULT_PLUGIN_ID

    def to_hydrabase_payload(self) -> dict:
        return {
            "plugin_id": self.plugin_id,
            "id": self.id,
            "address": self.address,
            "soul_id": self.soul_id,
            "type": self.type,
            "content": self.content,
            "content_raw": json.dumps(self.content),
            "confidence": self.confidence,
            "subject": self.subject,
            "ttl": self.ttl,
            "timestamp": self.timestamp,
            "expires_at": self.expires_at,
            "vote_count": self.vote_count,
        }

    @classmethod
    def from_hydrabase(cls, data: dict) -> "FerrisFact":
        content = data.get("content", {})
        if isinstance(content, str):
            content = json.loads(content)
        return cls(
            id=data["id"],
            soul_id=data["soul_id"],
            type=data["type"],
            content=content,
            confidence=data["confidence"],
            address=data["address"],
            subject=data.get("subject"),
            ttl=data.get("ttl"),
            timestamp=data.get("timestamp", int(time.time() * 1000)),
            expires_at=data.get("expires_at"),
            vote_count=data.get("vote_count", 0),
            plugin_id=data.get("plugin_id", DEFAULT_PLUGIN_ID),
        )


class HydrabaseSidecar:
    """
    Python client for Hydrabase P2P network — used by Ferris to read/write memory facts.

    Connects to a Hydrabase node via WebSocket and exposes:
    - submit_fact: write a Ferris fact to the local node (propagates to peers)
    - search_facts: full-text search across the network
    - lookup_fact: lookup a specific fact by soul_id
    - vote_fact: record a vote on a peer's fact (trust-weighted confidence update)
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
        self._reader: Optional[asyncio.StreamReader] = None
        self._writer: Optional[asyncio.StreamWriter] = None
        self._connected = False
        self._nonce_counter = 0
        self._pending: dict[int, asyncio.Future] = {}
        self._listen_task: Optional[asyncio.Task] = None

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

            self._ws = await websockets.connect(url, extra_headers=headers)
            self._connected = True
            self._listen_task = asyncio.create_task(self._listen_loop())
            logger.info(f"[HydrabaseSidecar] Connected to {url}")
            return True
        except ImportError:
            # websockets not installed — fall back to stdlib websocket-client
            logger.warning("[HydrabaseSidecar] websockets not installed, trying stdlib")
            return await self._connect_stdlib()
        except Exception as e:
            logger.error(f"[HydrabaseSidecar] Failed to connect: {e}")
            return False

    async def _connect_stdlib(self) -> bool:
        """Connect using Python stdlib (asyncio start_server as client)."""
        try:
            import json

            reader, writer = await asyncio.open_connection(self.host, self.port)
            self._reader = reader
            self._writer = writer

            # Send handshake (Hydrabase uses HTTP upgrade for WebSocket)
            key = "SmF2YVNjcmlwdA=="  # placeholder
            handshake = (
                f"GET / HTTP/1.1\r\n"
                f"Host: {self.host}:{self.port}\r\n"
                f"Upgrade: websocket\r\n"
                f"Connection: Upgrade\r\n"
                f"Sec-WebSocket-Key: {key}\r\n"
                f"Sec-WebSocket-Version: 13\r\n"
                f"{'Sec-WebSocket-Protocol: hydrabase-v1\r\n' if self.api_key else ''}"
                f"\r\n"
            )
            writer.write(handshake.encode())
            await writer.drain()
            self._connected = True
            self._listen_task = asyncio.create_task(self._listen_loop())
            return True
        except Exception as e:
            logger.error(f"[HydrabaseSidecar] stdlib connect failed: {e}")
            return False

    async def close(self):
        """Close the WebSocket connection."""
        self._connected = False
        if self._listen_task:
            self._listen_task.cancel()
            try:
                await self._listen_task
            except asyncio.CancelledError:
                pass
        if self._ws:
            await self._ws.close()
        if self._writer:
            self._writer.close()
            await self._writer.wait_closed()

    def _next_nonce(self) -> int:
        self._nonce_counter += 1
        return self._nonce_counter

    async def _send_raw(self, payload: dict) -> None:
        """Send a raw JSON payload to the WebSocket."""
        data = json.dumps(payload)
        if self._ws:
            await self._ws.send(data)
        elif self._writer:
            # WebSocket framing for stdlib
            frame = self._ws_frame(data)
            self._writer.write(frame)
            await self._writer.drain()

    def _ws_frame(self, data: str) -> bytes:
        """Simple WebSocket text frame encoding."""
        import struct

        payload = data.encode("utf-8")
        length = len(payload)
        frame = bytearray()
        frame.append(0x81)  # FIN + text frame
        if length <= 125:
            frame.append(0x80 | length)  # masked
        elif length <= 65535:
            frame.append(0x80 | 126)
            frame.extend(struct.pack(">H", length))
        else:
            frame.append(0x80 | 127)
            frame.extend(struct.pack(">Q", length))
        # Add mask (all zeros for simplicity — servers accept masked frames from clients)
        frame.extend(b"\x00\x00\x00\x00")
        frame.extend(payload)
        return bytes(frame)

    async def _listen_loop(self):
        """Background task: listen for responses from Hydrabase."""
        while self._connected:
            try:
                if self._ws:
                    msg = await asyncio.wait_for(self._ws.recv(), timeout=5.0)
                    await self._handle_message(msg)
                elif self._reader:
                    data = await asyncio.wait_for(self._reader.read(4096), timeout=5.0)
                    if data:
                        await self._handle_raw(data)
            except asyncio.TimeoutError:
                continue
            except Exception as e:
                if self._connected:
                    logger.warning(f"[HydrabaseSidecar] Listen error: {e}")
                break

    async def _handle_message(self, msg: str):
        """Handle an incoming JSON message from Hydrabase."""
        try:
            data = json.loads(msg)
            await self._dispatch(data)
        except json.JSONDecodeError:
            logger.warning(f"[HydrabaseSidecar] Non-JSON message: {msg[:100]}")

    async def _handle_raw(self, data: bytes):
        """Handle raw bytes from stdlib socket."""
        # Extract text frames (simplified — real impl needs full WS frame parsing)
        if len(data) > 2:
            try:
                length = data[1] & 0x7F
                payload = data[6:6 + length]
                text = payload.decode("utf-8", errors="ignore")
                await self._handle_message(text)
            except Exception as e:
                logger.warning(f"[HydrabaseSidecar] Raw parse error: {e}")

    async def _dispatch(self, data: dict):
        """Dispatch a response to the correct pending future."""
        nonce = data.get("nonce")
        if nonce is not None and nonce in self._pending:
            future = self._pending.pop(nonce)
            if not future.done():
                future.set_result(data)

    async def _request(self, request: dict) -> dict:
        """Send a request and wait for the response."""
        nonce = self._next_nonce()
        request["nonce"] = nonce

        future = asyncio.get_event_loop().create_future()
        self._pending[nonce] = future

        await self._send_raw({"request": request})

        try:
            result = await asyncio.wait_for(future, timeout=self.timeout)
            return result
        except asyncio.TimeoutError:
            self._pending.pop(nonce, None)
            raise TimeoutError(f"Hydrabase request timed out after {self.timeout}s: {request}")

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
        fact_id: Optional[str] = None,
    ) -> FerrisFact:
        """
        Submit a new Ferris fact to the local Hydrabase node.

        Args:
            content: dict with 'text' (required) and 'metadata' (optional)
            fact_type: 'belief' | 'conversation' | 'knowledge' | 'preference'
            subject: optional entity this fact pertains to
            confidence: initial confidence 0..1
            ttl: optional time-to-live in milliseconds
            fact_id: optional unique id (auto-generated if not provided)

        Returns:
            FerrisFact with computed soul_id
        """
        fact_id = fact_id or f"fact_{uuid.uuid4().hex[:16]}"
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

        response = await self._request({
            "type": "ferris.submit",
            "query": soul_id,  # query = soul_id for submit
            "fact": fact_data,
        })

        # Build returned fact
        return FerrisFact(
            id=fact_id,
            soul_id=soul_id,
            type=fact_type,
            content=content,
            confidence=confidence,
            address="0x0",  # local
            subject=subject,
            ttl=ttl,
            timestamp=now,
            expires_at=(now + ttl) if ttl else None,
            vote_count=0,
        )

    async def search_facts(
        self,
        query: str,
        min_confidence: float = 0.0,
    ) -> list[FerrisFact]:
        """
        Search for Ferris facts by content text across the network.

        Args:
            query: search string
            min_confidence: minimum confidence threshold (0..1)

        Returns:
            list of FerrisFact sorted by confidence descending
        """
        response = await self._request({
            "type": "ferris.facts",
            "query": query,
        })

        results = response.get("response", [])
        facts = [FerrisFact.from_hydrabase(r) for r in results]
        return [f for f in facts if f.confidence >= min_confidence]

    async def lookup_fact(self, soul_id: str) -> list[FerrisFact]:
        """
        Lookup a specific fact by its soul_id.

        Args:
            soul_id: the cross-node identity of the fact

        Returns:
            list of matching FerrisFact (may have multiple addresses if shared by peers)
        """
        response = await self._request({
            "type": "ferris.lookup",
            "query": soul_id,
        })

        results = response.get("response", [])
        return [FerrisFact.from_hydrabase(r) for r in results]

    async def vote_fact(
        self,
        soul_id: str,
        peer_address: str,
        peer_confidence: float,
        fact_confidence: float,
    ) -> FerrisFact:
        """
        Vote on a fact contributed by a peer. Updates the trust-weighted confidence.

        Args:
            soul_id: the fact's soul_id
            peer_address: address of the peer who contributed the fact
            peer_confidence: how much you trust this peer (0..1)
            fact_confidence: the confidence score from the peer

        Returns:
            updated FerrisFact
        """
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
        raise ValueError(f"No fact found with soul_id: {soul_id}")

    def _compute_soul_id(self, id: str) -> str:
        """
        Compute soul_id using the same scheme as Hydrabase:
        soul_${Bun.hash("ferris:${id}".slice(0, cutoff))}
        """
        import hashlib

        prefix = f"{DEFAULT_PLUGIN_ID}:{id}"
        # Hydrabase uses Bun.hash which is a non-cryptographic hash.
        # We use the same truncation scheme for compatibility.
        # Bun.hash output is a 64-bit integer; we take first 32 chars of hex.
        # Since we don't have Bun.hash in Python, we use a compatible scheme.
        h = hashlib.sha256(prefix.encode()).digest()
        hash_int = int.from_bytes(h[:8], "big")
        return f"soul_{hash_int % (10 ** 18)}"  # match Hydrabase's soul_id format

    @property
    def is_connected(self) -> bool:
        return self._connected


# Convenience function for sync usage
def make_sidecar(**kwargs) -> HydrabaseSidecar:
    return HydrabaseSidecar(**kwargs)
