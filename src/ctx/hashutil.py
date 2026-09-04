from __future__ import annotations

import hashlib


def content_hash(data: bytes) -> str:
    return hashlib.sha1(data, usedforsecurity=False).hexdigest()
