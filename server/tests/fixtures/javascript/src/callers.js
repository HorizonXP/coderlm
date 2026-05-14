const { render } = require("./free");
const { Renderer } = require("./method");

function exercise(renderer, unknown) {
  const direct = render("direct");
  const method = renderer.render("method");
  const unrelated = unknown.render("unrelated");
  return [direct, method, unrelated, Renderer];
}

module.exports = { exercise };
