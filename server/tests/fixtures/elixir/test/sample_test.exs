defmodule Fixture.SampleTest do
  use ExUnit.Case, async: true

  test "public_fun calls normalize" do
    assert Fixture.Sample.public_fun(" alice ", [1]) == [{"alice", 1}]
  end

  test "guarded can be referenced separately" do
    assert Fixture.Sample.guarded(1) == 2
  end
end
