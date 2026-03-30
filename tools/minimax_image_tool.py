#!/usr/bin/env python3
"""
MiniMax Image Generation Tool

Provides image generation using MiniMax's image-01 model via the
OpenAI-compatible /v1/image_generation endpoint.

Uses MINIMAX_API_KEY and optionally MINIMAX_BASE_URL (defaults to
https://api.minimax.io/v1).

Requires:
- MINIMAX_API_KEY env var
- requests library (stdlib urllib used, no extra dep)
"""

import json
import logging
import os
import datetime
from typing import Dict, Any, Optional, Union

logger = logging.getLogger(__name__)

# Configuration
DEFAULT_MODEL = "image-01"
DEFAULT_BASE_URL = "https://api.minimax.io/v1"
DEFAULT_ASPECT_RATIO = "landscape"
DEFAULT_NUM_IMAGES = 1

# Aspect ratio → MiniMax image_size
ASPECT_RATIO_MAP = {
    "landscape": "1024x1024",
    "square":    "1024x1024",
    "portrait":  "1024x1024",
}
VALID_ASPECT_RATIOS = list(ASPECT_RATIO_MAP.keys())


def _check_minimax_api_key() -> bool:
    return bool(os.getenv("MINIMAX_API_KEY"))


def _build_client() -> tuple[str, str]:
    """Returns (base_url, api_key)."""
    base_url = os.getenv("MINIMAX_BASE_URL", DEFAULT_BASE_URL).rstrip("/")
    api_key = os.getenv("MINIMAX_API_KEY", "")
    return base_url, api_key


def _generate_minimax(
    prompt: str,
    model: str = DEFAULT_MODEL,
    image_size: str = "1024x1024",
    num_images: int = DEFAULT_NUM_IMAGES,
    **kwargs,
) -> Dict[str, Any]:
    """Call MiniMax /v1/image_generation and return parsed JSON response."""
    import urllib.request
    import urllib.error

    base_url, api_key = _build_client()
    url = f"{base_url}/image_generation"

    payload = {
        "model": model,
        "prompt": prompt.strip(),
        "image_size": image_size,
        "num_images": num_images,
    }
    for k, v in kwargs.items():
        if v is not None and v != "":
            payload[k] = v

    body = json.dumps(payload).encode("utf-8")
    req = urllib.request.Request(
        url,
        data=body,
        headers={
            "Authorization": f"Bearer {api_key}",
            "Content-Type": "application/json",
        },
        method="POST",
    )

    try:
        with urllib.request.urlopen(req, timeout=120) as resp:
            return json.loads(resp.read().decode("utf-8"))
    except urllib.error.HTTPError as e:
        body = e.read().decode("utf-8") if e.fp else ""
        raise RuntimeError(f"MiniMax API error {e.code}: {body}") from e
    except Exception as e:
        raise RuntimeError(f"MiniMax request failed: {e}") from e


def minimax_image_tool(
    prompt: str,
    aspect_ratio: str = DEFAULT_ASPECT_RATIO,
    num_images: int = DEFAULT_NUM_IMAGES,
    model: str = DEFAULT_MODEL,
) -> str:
    """
    Generate images from text prompts using MiniMax's image-01 model.

    Args:
        prompt (str): Text description of the desired image.
        aspect_ratio (str): "landscape", "square", or "portrait" (default: "landscape").
        num_images (int): Number of images to generate, 1-4 (default: 1).
        model (str): Model name — currently "image-01" (default).

    Returns:
        JSON string:
            {"success": true,  "image": "<url>"}
            {"success": false, "image": null, "error": "..."}
    """
    start = datetime.datetime.now()

    if not prompt or not isinstance(prompt, str) or not prompt.strip():
        return json.dumps({"success": False, "image": None, "error": "prompt is required"})

    if not _check_minimax_api_key():
        return json.dumps({
            "success": False, "image": None,
            "error": "MINIMAX_API_KEY environment variable not set",
        })

    ar = aspect_ratio.lower().strip() if aspect_ratio else DEFAULT_ASPECT_RATIO
    if ar not in ASPECT_RATIO_MAP:
        ar = DEFAULT_ASPECT_RATIO
    image_size = ASPECT_RATIO_MAP[ar]

    if not isinstance(num_images, int) or num_images < 1 or num_images > 4:
        return json.dumps({
            "success": False, "image": None,
            "error": "num_images must be an integer between 1 and 4",
        })

    try:
        logger.info("Generating %d image(s) with MiniMax image-01: %s", num_images, prompt[:60])
        resp = _generate_minimax(
            prompt=prompt,
            model=model,
            image_size=image_size,
            num_images=num_images,
        )

        base_resp = resp.get("base_resp", {})
        if base_resp.get("status_code", 0) != 0:
            return json.dumps({
                "success": False, "image": None,
                "error": base_resp.get("status_msg", "unknown error"),
            })

        image_urls = resp.get("data", {}).get("image_urls", [])
        elapsed = (datetime.datetime.now() - start).total_seconds()

        if image_urls:
            logger.info("Got %d image(s) in %.1fs", len(image_urls), elapsed)
            return json.dumps({"success": True, "image": image_urls[0]})
        else:
            return json.dumps({
                "success": False, "image": None,
                "error": "no images in response",
            })

    except Exception as e:
        logger.error("MiniMax image generation failed: %s", e)
        return json.dumps({"success": False, "image": None, "error": str(e)})


def check_minimax_image_requirements() -> bool:
    """Return True if MINIMAX_API_KEY is set."""
    return _check_minimax_api_key()


# ---------------------------------------------------------------------------
# Registry
# ---------------------------------------------------------------------------
from tools.registry import registry

MINIMAX_IMAGE_SCHEMA = {
    "name": "minimax_image",
    "description": "Generate images using MiniMax's image-01 model. "
        "Good quality, fast generation. Use when FAL_KEY is unavailable or "
        "you want to use your existing MiniMax API key. "
        "Returns a single image URL. Display it using markdown: ![description](URL)",
    "parameters": {
        "type": "object",
        "properties": {
            "prompt": {
                "type": "string",
                "description": "The text prompt describing the desired image. Be detailed and descriptive."
            },
            "aspect_ratio": {
                "type": "string",
                "enum": ["landscape", "square", "portrait"],
                "description": "The aspect ratio of the generated image. All three produce 1024x1024 output.",
                "default": "landscape"
            },
            "num_images": {
                "type": "integer",
                "minimum": 1,
                "maximum": 4,
                "description": "Number of images to generate (1-4).",
                "default": 1
            },
        },
        "required": ["prompt"]
    }
}


def _handle_minimax_image(args, **kw):
    return minimax_image_tool(
        prompt=args.get("prompt", ""),
        aspect_ratio=args.get("aspect_ratio", "landscape"),
        num_images=args.get("num_images", 1),
    )


registry.register(
    name="minimax_image",
    toolset="image_gen",
    schema=MINIMAX_IMAGE_SCHEMA,
    handler=_handle_minimax_image,
    check_fn=check_minimax_image_requirements,
    requires_env=["MINIMAX_API_KEY"],
)
