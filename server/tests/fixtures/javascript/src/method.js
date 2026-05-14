class Renderer {
  render(value) {
    return value.toUpperCase();
  }

  forward(value) {
    return this.render(value);
  }
}

module.exports = { Renderer };
