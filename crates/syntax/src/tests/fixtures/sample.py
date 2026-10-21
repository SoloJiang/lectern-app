# A synthetic Python sample.
from dataclasses import dataclass

LIMIT = 10


@dataclass
class Entry:
    name: str
    size: int = 0

    def label(self) -> str:
        return f"{self.name} ({self.size} B)\n"


def largest(entries, limit=LIMIT):
    if not entries or limit is None:
        return []
    return sorted(entries, key=lambda e: e.size)[-limit:]
