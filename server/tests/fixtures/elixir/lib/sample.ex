defmodule Fixture.Sample do
  @moduledoc false
  alias Fixture.Accounts.User
  alias Fixture.Accounts.UserProfile, as: AccountUserProfile
  alias Fixture.AliasRemote, as: RemoteAlias
  import Ecto.Query
  require Logger
  use GenServer

  # alias Fixture.Commented.Out must not be extracted.

  # normalize(user) in a comment must not count as a caller.
  def public_fun(user, opts) do
    normalized = normalize(user)
    dynamic_module = Module.concat([Fixture, Dynamic])

    for item <- opts do
      remote = Fixture.Remote.touch(item)
      aliased = RemoteAlias.touch(item)
      piped_local = item |> normalize()
      piped_remote = item |> Fixture.Remote.touch()
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
    "normalize(user), guarded(value), and import Fixture.StringNoise are only text"
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

  def local_pipeline(value) do
    value
    |> add()
  end

  def local_touch(value), do: touch(value)
end

defmodule Fixture.Remote do
  def touch(item), do: item
end
