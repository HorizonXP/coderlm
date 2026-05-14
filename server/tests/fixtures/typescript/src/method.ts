export class Renderer {
  render(value: string): string {
    return value.toUpperCase();
  }

  forward(value: string): string {
    return this.render(value);
  }
}
