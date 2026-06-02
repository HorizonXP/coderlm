from analytics.pipeline import orchestrate_report


def test_orchestrate_report() -> None:
    assert orchestrate_report([1, 2, 3]) == {"event_count": 3, "score": 12}
