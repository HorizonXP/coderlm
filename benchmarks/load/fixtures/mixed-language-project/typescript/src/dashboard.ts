export const DASHBOARD_SENTINEL = "dashboard-original-sentinel";

export function formatMetric(label: string, value: number): string {
  return `${label}:${value}`;
}

export function renderDashboard(values: number[]): string[] {
  return values.map((value, index) => formatMetric(`metric-${index}`, value));
}
