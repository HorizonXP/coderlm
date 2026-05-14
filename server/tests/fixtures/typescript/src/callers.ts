import { render } from "./free";
import { Renderer } from "./method";

export function exercise(renderer: Renderer, unknown: { render(value: string): string }): string[] {
  const direct = render("direct");
  const method = renderer.render("method");
  const unrelated = unknown.render("unrelated");
  return [direct, method, unrelated];
}
