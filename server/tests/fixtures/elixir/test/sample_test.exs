defmodule Fixture.SampleTest do
  use ExUnit.Case, async: true

  setup do
    Fixture.Sample.public_fun("setup", [1])
    :ok
  end

  describe "public_fun behavior" do
    describe "nested context" do
      test "calls normalize directly" do
        assert Fixture.Sample.public_fun(" alice ", [1]) == [{"alice", 1}]
      end

      test "description mentions guarded but body does not" do
        assert true
      end
    end
  end

  describe "guarded context without matching test body" do
    test "uses a different helper" do
      assert helper_for_public_fun() == [{"bob", 2}]
    end
  end

  test "guarded can be referenced separately" do
    assert Fixture.Sample.guarded(1) == 2
  end

  test "public_fun appears only in description" do
    assert true
  end

  test "helper call stays conservative for public_fun" do
    assert helper_for_public_fun() == [{"bob", 2}]
  end

  defp helper_for_public_fun do
    Fixture.Sample.public_fun(" bob ", [2])
  end
end
