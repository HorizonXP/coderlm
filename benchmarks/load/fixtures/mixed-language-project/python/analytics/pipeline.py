"""Python targets for the mixed-language fixture."""

PIPELINE_LABEL = "mixed-language-pipeline"


def normalize_event(value: int) -> int:
    return value * 2


def orchestrate_report(events: list[int]) -> dict[str, int]:
    normalized = [normalize_event(event) for event in events]
    return {"event_count": len(events), "score": sum(normalized)}
