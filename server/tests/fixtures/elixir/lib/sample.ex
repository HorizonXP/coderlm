defmodule Fixture.Sample do
  @moduledoc false
  alias Fixture.Accounts.User
  alias Fixture.Accounts.UserProfile, as: AccountUserProfile
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
end

defmodule Fixture.Remote do
  def touch(item), do: item
end
