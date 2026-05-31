from starter.math_ops import compute_total, summarize_invoice


def test_compute_total() -> None:
    assert compute_total([2, 3, 5]) == 10


def test_summarize_invoice() -> None:
    assert summarize_invoice([1, 4]) == {"line_count": 2, "total": 5}
