#!/usr/bin/env python3
"""
Mock LLM server for benchmarking compress_async vs Python compress.
Simulates network + LLM latency.
"""
import asyncio
import time
from fastapi import FastAPI
from pydantic import BaseModel
from typing import Optional, List, Dict, Any

app = FastAPI()

LLM_DELAY = 0.1  # 100ms simulated LLM latency — tweak this

class Message(BaseModel):
    role: str
    content: Optional[str] = None
    name: Optional[str] = None
    tool_calls: Optional[List[Dict[str, Any]]] = None

class ChatRequest(BaseModel):
    model: str
    messages: List[Message]
    temperature: float = 0.7
    max_tokens: int = 1024

class MessageContent(BaseModel):
    role: str
    content: str

class Choice(BaseModel):
    message: MessageContent
    finish_reason: str = "stop"
    index: int = 0

class Usage(BaseModel):
    prompt_tokens: int
    completion_tokens: int
    total_tokens: int

class ChatResponse(BaseModel):
    id: str = "mock"
    object: str = "chat.completion"
    created: int
    model: str
    choices: List[Choice]
    usage: Usage

@app.post("/v1/chat/completions")
async def chat_completions(req: ChatRequest):
    await asyncio.sleep(LLM_DELAY)  # Simulate LLM latency

    # Summarize the messages as "[CONTEXT COMPACTION] ..."
    total_content = ""
    for m in req.messages:
        if m.content:
            total_content += f"{m.role}: {m.content[:100]}... "
    
    return ChatResponse(
        created=int(time.time()),
        model=req.model,
        choices=[
            Choice(
                message=MessageContent(
                    role="assistant",
                    content=f"[CONTEXT COMPACTION] Summary of {len(req.messages)} messages: {total_content[:200]}..."
                )
            )
        ],
        usage=Usage(
            prompt_tokens=len(total_content) // 4,
            completion_tokens=50,
            total_tokens=len(total_content) // 4 + 50,
        )
    ).model_dump()

if __name__ == "__main__":
    import uvicorn
    uvicorn.run(app, host="127.0.0.1", port=18999, log_level="error")
