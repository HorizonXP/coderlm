"""Known Python targets for load fixture smoke tests."""

STARTER_GREP_TARGET = "starter-fixture-grep"
MUTATION_SENTINEL = "starter-original-sentinel"


def compute_total(items: list[int]) -> int:
    """Return a deterministic total for invoice line items."""

    return sum(items)


def summarize_invoice(items: list[int]) -> dict[str, int]:
    total = compute_total(items)
    return {"line_count": len(items), "total": total}


class InvoiceCalculator:
    def add_fee(self, amount: int, fee: int) -> int:
        return amount + fee
