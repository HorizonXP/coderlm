defmodule Fixture.Sample do
  @moduledoc false

  # normalize(user) in a comment must not count as a caller.
  def public_fun(user, opts) do
    normalized = normalize(user)

    for item <- opts do
      remote = Fixture.Remote.touch(item)
      {normalized, remote}
    end
  end

  def guarded(value) when is_integer(value) do
    value + 1
  end

  defp normalize(user) do
    String.trim(user)
  end

  def string_noise do
    "normalize(user) and guarded(value) are only text"
  end

  def add(value), do: value + 1

  def add(left, right), do: left + right

  def multi_clause(:ok), do: :ok
  def multi_clause(:error), do: :error

  def pattern_count({left, right}, [head | _tail], %{flag: flag}) when flag do
    {left, right, head}
  end

  def with_default(value, opts \\ []), do: {value, opts}

  defdelegate delegated(value), to: Fixture.Remote, as: :touch
end

defmodule Fixture.Remote do
  def touch(item), do: item
end
