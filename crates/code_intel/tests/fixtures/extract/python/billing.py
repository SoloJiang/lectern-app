"""Invoices and payments."""

from dataclasses import dataclass
from decimal import Decimal

TAX_RATE = Decimal("0.2")
_registry = {}


def register(kind):
    def wrap(cls):
        _registry[kind] = cls
        return cls

    return wrap


@dataclass
class Line:
    description: str
    amount: Decimal

    def total(self, tax=TAX_RATE):
        return self.amount * (1 + tax)


@register("invoice")
class Invoice(Base):
    def __init__(self, number, *lines, currency="EUR", **extra):
        self.number = number
        self.lines = list(lines)
        self.currency = currency

    @property
    def total(self):
        subtotal = sum(line.total() for line in self.lines)
        return round(subtotal, 2)

    @staticmethod
    def parse(text, /, strict=False, *, encoding="utf-8"):
        fields = text.split(",")
        return Invoice(fields[0])


def pay(invoice, amount, callback=lambda paid: None):
    remaining = invoice.total - amount
    callback(remaining <= 0)
    return remaining
