defmodule Trackline.EmailInviteFlowAcceptanceTest do
  use TracklineWeb.ConnCase, async: false

  import Ecto.Query
  import Trackline.AccountsFixtures
  import Trackline.SupportFixtures

  alias Trackline.{Repo, Support}
  alias Trackline.Support.Membership

  setup do
    previous_clock = Application.fetch_env(:trackline, :clock)
    now = DateTime.utc_now() |> DateTime.truncate(:second)
    set_clock(now)

    on_exit(fn ->
      case previous_clock do
        {:ok, clock} -> Application.put_env(:trackline, :clock, clock)
        :error -> Application.delete_env(:trackline, :clock)
      end
    end)

    owner = new_user()
    org = organization_fixture(owner)
    %{owner: owner, org: org, now: now}
  end

  @tag acceptance: "A1"
  test "owners deliver normalized role invitations with nonrecoverable tokens", %{owner: owner, org: org} do
    tokens =
      for role <- ["agent", "viewer"] do
        recipient = new_user()
        address = "  " <> String.upcase(recipient.email) <> "  "
        assert {:ok, invitation} = invite_member(org, owner, address, role)
        assert invitation.id
        token = emailed_token(recipient.email)
        assert Regex.match?(~r/\A[A-Za-z0-9_-]{20,}\z/, token)
        assert_tokens_not_persisted([token])
        assert {:ok, membership} = accept_invite(token, recipient)
        assert membership.role == role
        assert membership.organization_id == org.id
        assert membership.user_id == recipient.id
        token
      end

    assert length(Enum.uniq(tokens)) == 2
  end

  @tag acceptance: "A2"
  test "refused invitation and resend requests send no email", %{owner: owner, org: org} do
    agent = member_fixture(org, "agent")
    viewer = member_fixture(org, "viewer")
    outsider = new_user()
    organization_fixture(outsider)
    recipient = new_user()
    drain_emails()

    for actor <- [agent, viewer, outsider] do
      assert {:error, _} = invite_member(org, actor, recipient.email, "agent")
      refute_receive {:email, _}
    end

    for address <- ["", "   ", "not-an-address", "@example.com", "two@example.com extra"] do
      assert {:error, _} = invite_member(org, owner, address, "agent")
      refute_receive {:email, _}
    end

    for role <- ["owner", "admin", "", nil] do
      assert {:error, _} = invite_member(org, owner, recipient.email, role)
      refute_receive {:email, _}
    end

    assert {:ok, invitation} = invite_member(org, owner, recipient.email, "viewer")
    token = emailed_token(recipient.email)

    for actor <- [agent, viewer, outsider] do
      assert {:error, _} = resend_invite(invitation, actor)
      refute_receive {:email, _}
    end

    assert {:ok, membership} = accept_invite(token, recipient)
    assert membership.role == "viewer"
  end

  @tag acceptance: "A3"
  test "acceptance is recipient-bound, single-use, and preserves existing membership", %{owner: owner, org: org} do
    recipient = new_user()
    wrong_user = new_user()
    address = " " <> String.upcase(recipient.email) <> " "
    assert {:ok, _} = invite_member(org, owner, address, "agent")
    token = emailed_token(recipient.email)

    assert {:error, _} = accept_invite(token, wrong_user)
    assert Support.get_membership(org, wrong_user) == nil

    # Exercise normalization of the signed-in user's persisted address too.
    recipient =
      recipient
      |> Ecto.Changeset.change(email: address)
      |> Repo.update!()

    assert {:ok, membership} = accept_invite(token, recipient)
    assert membership.role == "agent"
    assert membership.organization_id == org.id
    assert membership.user_id == recipient.id
    assert {:error, _} = accept_invite(token, recipient)
    assert membership_count(org, recipient) == 1

    for existing_role <- ["owner", "agent", "viewer"] do
      existing_user = if existing_role == "owner", do: owner, else: member_fixture(org, existing_role)
      drain_emails()
      original = Support.get_membership(org, existing_user)
      assert {:ok, _} = invite_member(org, owner, existing_user.email, "viewer")
      existing_token = emailed_token(existing_user.email)
      result = accept_invite(existing_token, existing_user)

      assert match?({:ok, %Membership{}}, result) or match?({:error, _}, result)
      assert membership_count(org, existing_user) == 1
      current = Support.get_membership(org, existing_user)
      assert current.id == original.id
      assert current.role == existing_role
    end

    for invalid_token <- ["", "not-a-real-token", "***invalid***"] do
      assert {:error, _} = accept_invite(invalid_token, recipient)
    end
  end

  @tag acceptance: "A4"
  test "seven-day expiry and resend rotation use Clock and reject consumed invitations", %{owner: owner, org: org, now: now} do
    recipient = new_user()
    near_expiry_user = new_user()
    assert {:ok, original} = invite_member(org, owner, recipient.email, "viewer")
    old_token = emailed_token(recipient.email)
    assert {:ok, used_invitation} = invite_member(org, owner, near_expiry_user.email, "agent")
    near_expiry_token = emailed_token(near_expiry_user.email)

    set_clock(DateTime.add(now, 7 * 86_400 - 1, :second))
    assert {:ok, _} = accept_invite(near_expiry_token, near_expiry_user)

    set_clock(DateTime.add(now, 7 * 86_400, :second))
    assert {:error, _} = accept_invite(old_token, recipient)
    assert Support.get_membership(org, recipient) == nil
    assert {:error, _} = resend_invite(used_invitation, owner)
    refute_receive {:email, _}

    # The caller still has the original object, not a reloaded invitation.
    assert {:ok, refreshed} = resend_invite(original, owner)
    new_token = emailed_token(recipient.email)
    refute new_token == old_token
    assert {:error, _} = accept_invite(old_token, recipient)
    assert_tokens_not_persisted([old_token, new_token])

    # Resend an unexpired invitation too: every resend must rotate its link.
    assert {:ok, newest} = resend_invite(refreshed, owner)
    newest_token = emailed_token(recipient.email)
    refute newest_token in [old_token, new_token]
    assert {:error, _} = accept_invite(new_token, recipient)
    assert_tokens_not_persisted([old_token, new_token, newest_token])

    set_clock(DateTime.add(now, 14 * 86_400 - 1, :second))
    assert {:ok, membership} = accept_invite(newest_token, recipient)
    assert membership.role == "viewer"
    assert membership.organization_id == org.id
    assert {:error, _} = resend_invite(newest, owner)
    assert {:error, _} = resend_invite(original, owner)
    refute_receive {:email, _}

    # Prove the renewed window also expires at its exact seven-day boundary.
    boundary_user = new_user()
    assert {:ok, boundary_invitation} = invite_member(org, owner, boundary_user.email, "agent")
    emailed_token(boundary_user.email)
    sent_at = Trackline.Clock.now()
    assert {:ok, _} = resend_invite(boundary_invitation, owner)
    boundary_token = emailed_token(boundary_user.email)
    set_clock(DateTime.add(sent_at, 7 * 86_400, :second))
    assert {:error, _} = accept_invite(boundary_token, boundary_user)
    assert Support.get_membership(org, boundary_user) == nil
  end

  @tag acceptance: "A5"
  test "GET invite authenticates, accepts, and reports unusable links", %{owner: owner, org: org, now: now} do
    recipient = new_user()
    wrong_user = new_user()
    expired_user = new_user()
    assert {:ok, _} = invite_member(org, owner, recipient.email, "agent")
    token = emailed_token(recipient.email)

    signed_out = get(build_conn(), "/invites/#{token}")
    assert redirected_to(signed_out) == "/users/log-in"
    assert Support.get_membership(org, recipient) == nil

    wrong = build_conn() |> log_in_user(wrong_user) |> get("/invites/#{token}")
    assert_invite_error(wrong)
    assert Support.get_membership(org, wrong_user) == nil

    accepted = build_conn() |> log_in_user(recipient) |> get("/invites/#{token}")
    assert redirected_to(accepted) == "/orgs/#{org.slug}/tickets"
    assert Support.get_membership(org, recipient).role == "agent"
    assert membership_count(org, recipient) == 1

    replay = build_conn() |> log_in_user(recipient) |> get("/invites/#{token}")
    assert_invite_error(replay)
    assert membership_count(org, recipient) == 1

    invalid = build_conn() |> log_in_user(recipient) |> get("/invites/not-a-real-token")
    assert_invite_error(invalid)

    assert {:ok, _} = invite_member(org, owner, expired_user.email, "viewer")
    expired_token = emailed_token(expired_user.email)
    set_clock(DateTime.add(now, 7 * 86_400, :second))
    expired = build_conn() |> log_in_user(expired_user) |> get("/invites/#{expired_token}")
    assert_invite_error(expired)
    assert Support.get_membership(org, expired_user) == nil
  end

  @tag acceptance: "A6"
  test "existing owner creation, membership uniqueness, and viewer permissions remain intact", %{owner: owner, org: org} do
    owner_membership = Support.get_membership(org, owner)
    assert owner_membership.role == "owner"
    assert Support.can_write?(owner_membership)
    assert {:error, %Ecto.Changeset{}} = Support.add_member(org, owner, "viewer")
    assert membership_count(org, owner) == 1
    assert Support.get_membership(org, owner).role == "owner"

    agent = member_fixture(org, "agent")
    viewer = member_fixture(org, "viewer")
    assert Support.can_write?(Support.get_membership(org, agent))
    refute Support.can_write?(Support.get_membership(org, viewer))
    refute Support.can_write?(nil)
  end

  # Resolve ticket APIs at runtime: they do not yet exist on the base checkout.
  # Missing implementations fail an assertion; nothing is skipped or rescued.
  defp call_support(function, arguments) do
    assert Code.ensure_loaded?(Support)
    arity = length(arguments)
    assert function_exported?(Support, function, arity),
           "Expected Trackline.Support to implement #{function}/#{arity}"
    apply(Support, function, arguments)
  end

  defp invite_member(org, inviter, email, role),
    do: call_support(:invite_member, [org, inviter, email, role])

  defp resend_invite(invitation, actor),
    do: call_support(:resend_invite, [invitation, actor])

  defp accept_invite(token, user),
    do: call_support(:accept_invite, [token, user])

  defp new_user do
    user = user_fixture()
    drain_emails()
    user
  end

  defp drain_emails do
    receive do
      {:email, _} -> drain_emails()
    after
      0 -> :ok
    end
  end

  defp set_clock(now), do: Application.put_env(:trackline, :clock, fn -> now end)

  defp emailed_token(address) do
    assert_receive {:email, %Swoosh.Email{} = email}, 1_000
    assert [{name, ^address}] = email.to
    assert is_binary(name)
    assert is_binary(email.text_body)
    assert [_, token] = Regex.run(~r{/invites/([A-Za-z0-9_-]+)(?=\s|$)}, email.text_body)
    token
  end

  defp membership_count(org, user) do
    Repo.aggregate(
      from(m in Membership, where: m.organization_id == ^org.id and m.user_id == ^user.id),
      :count,
      :id
    )
  end

  defp assert_invite_error(conn) do
    assert redirected_to(conn) == "/orgs"
    error = Phoenix.Flash.get(conn.assigns.flash, :error)
    assert is_binary(error) and String.trim(error) != ""
  end

  defp assert_tokens_not_persisted(tokens) do
    # Inspect every table, including binary columns and their URL-safe encodings.
    tables =
      Repo.query!("SELECT name FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%'").rows

    for [table] <- tables do
      quoted = "\"" <> String.replace(table, "\"", "\"\"") <> "\""
      rows = Repo.query!("SELECT * FROM " <> quoted).rows

      for row <- rows, value <- row, is_binary(value), token <- tokens do
        refute :binary.match(value, token) != :nomatch,
               "raw invitation token persisted in #{table}"

        refute Base.url_encode64(value, padding: false) == token,
               "recoverable binary invitation token persisted in #{table}"

        refute Base.url_encode64(value, padding: true) == token,
               "recoverable padded binary invitation token persisted in #{table}"
      end
    end
  end
end
