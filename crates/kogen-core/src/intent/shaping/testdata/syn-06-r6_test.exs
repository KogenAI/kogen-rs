defmodule Trackline.TicketNumbersAcceptance.MigrationTest do
  use ExUnit.Case, async: false

  alias Trackline.Repo, as: MigrationRepo
  @baseline 20260929090005

  @tag acceptance: "A1"
  test "populated SQLite migration backfills deterministically and survives rollback/remigration" do
    database = Path.join(System.tmp_dir!(), "ticket-number-#{System.unique_integer([:positive, :monotonic])}.db")

    on_exit(fn ->
      for suffix <- ["", "-wal", "-shm", "-journal"] do
        File.rm(database <> suffix)
      end
    end)

    isolated_repo = start_supervised!({MigrationRepo, [
      name: nil,
      database: database,
      pool: DBConnection.ConnectionPool,
      pool_size: 2,
      journal_mode: :wal,
      busy_timeout: 5_000
    ]})
    previous_repo = MigrationRepo.put_dynamic_repo(isolated_repo)

    try do
      migrations = Application.app_dir(:trackline, "priv/repo/migrations")
      Ecto.Migrator.run(MigrationRepo, migrations, :up, to: @baseline, log: false)
      refute "number" in column_names("tickets")

      sql!("""
      INSERT INTO users (id, email, inserted_at, updated_at)
      VALUES (901, 'migration-number@example.com', '2026-01-01 00:00:00', '2026-01-01 00:00:00')
      """)

      for id <- [201, 202, 203] do
        sql!("""
        INSERT INTO organizations
          (id, name, slug, webhook_url, webhook_secret, inserted_at, updated_at)
        VALUES (?, ?, ?, ?, ?, '2026-01-01 00:00:00', '2026-01-01 00:00:00')
        """, [id, "Legacy #{id}", "legacy-#{id}", "https://example.com/#{id}", "secret-#{id}"])
      end

      sql!("""
      INSERT INTO memberships (organization_id, user_id, role, inserted_at, updated_at)
      VALUES (201, 901, 'owner', '2026-01-01 00:00:00', '2026-01-01 00:00:00')
      """)

      # Interleave organizations and make id order differ from creation order.
      for {id, org, created} <- [
            {700, 201, "2026-03-01 00:00:00"},
            {705, 202, "2026-02-01 00:00:00"},
            {710, 201, "2026-01-01 00:00:00"},
            {720, 201, "2026-01-01 00:00:00"},
            {730, 202, "2026-01-01 00:00:00"}
          ] do
        sql!("""
        INSERT INTO tickets
          (id, organization_id, author_id, assignee_id, title, body, status, priority, inserted_at, updated_at)
        VALUES (?, ?, 901, 901, ?, ?, 'closed', 'high', ?, '2026-04-01 00:00:00')
        """, [id, org, "Legacy #{id}", "Original body #{id}", created])

        sql!("""
        INSERT INTO comments (ticket_id, author_id, body, inserted_at, updated_at)
        VALUES (?, 901, ?, '2026-04-01 00:00:00', '2026-04-01 00:00:00')
        """, [id, "Preserve comment #{id}"])
      end

      tables = ["tickets", "comments", "organizations", "memberships", "users"]
      snapshots = Map.new(tables, fn table ->
        columns = column_names(table)
        {table, {columns, table_rows(table, columns), column_definitions(table)}}
      end)

      newer = Ecto.Migrator.run(MigrationRepo, migrations, :up, all: true, log: false)
      assert newer != [], "add migration versions newer than #{@baseline}"
      assert Enum.all?(newer, &(&1 > @baseline))
      assert "number" in column_names("tickets")

      expected = [[700, 201, 3], [705, 202, 2], [710, 201, 1], [720, 201, 2], [730, 202, 1]]
      assert sql!("SELECT id, organization_id, number FROM tickets ORDER BY id").rows == expected
      assert_snapshots(snapshots)

      # Same migrator operation used by mix ecto.rollback --step N.
      rolled_back = Ecto.Migrator.run(MigrationRepo, migrations, :down, step: length(newer), log: false)
      assert Enum.sort(rolled_back) == Enum.sort(newer)
      refute "number" in column_names("tickets")
      assert_snapshots(snapshots)
      assert column_definitions("tickets") == elem(snapshots["tickets"], 2)
      assert sql!("PRAGMA foreign_key_check").rows == []

      reapplied = Ecto.Migrator.run(MigrationRepo, migrations, :up, all: true, log: false)
      assert Enum.sort(reapplied) == Enum.sort(newer)
      assert sql!("SELECT id, organization_id, number FROM tickets ORDER BY id").rows == expected
      assert_snapshots(snapshots)
      assert sql!("PRAGMA foreign_key_check").rows == []
    after
      MigrationRepo.put_dynamic_repo(previous_repo)
    end
  end

  defp sql!(statement, params \\ []) do
    Ecto.Adapters.SQL.query!(MigrationRepo, statement, params)
  end

  defp column_names(table) do
    sql!("PRAGMA table_info(#{table})").rows |> Enum.map(&Enum.at(&1, 1))
  end

  defp column_definitions(table) do
    sql!("PRAGMA table_info(#{table})").rows
    |> Enum.map(fn [_position | definition] -> definition end)
    |> MapSet.new()
  end

  defp table_rows(table, columns) do
    projection = Enum.map_join(columns, ", ", fn column -> "\"#{column}\"" end)
    sql!("SELECT #{projection} FROM #{table} ORDER BY id").rows
  end

  defp assert_snapshots(snapshots) do
    for {table, {columns, rows, _definitions}} <- snapshots do
      assert Enum.all?(columns, &(&1 in column_names(table))), "lost columns from #{table}"
      assert table_rows(table, columns) == rows, "changed or lost rows from #{table}"
    end
  end
end

defmodule Trackline.TicketNumbersAcceptance.BehaviorTest do
  use TracklineWeb.ConnCase, async: false

  import Phoenix.LiveViewTest
  import Trackline.SupportFixtures

  alias Trackline.Repo
  alias Trackline.Support
  alias Trackline.Support.Ticket

  setup :register_and_log_in_user

  setup %{user: user} do
    %{org: organization_fixture(user)}
  end

  @tag acceptance: "A2"
  test "creation assigns permanent organization-local numbers without trusting callers or wasting numbers",
       %{user: user, org: org} do
    other = organization_fixture(user)
    {:ok, first} = Support.create_ticket(org, user, %{title: "First", number: 999})
    assert Map.get(first, :number) == 1
    assert Map.get(Support.get_ticket!(first.id), :number) == 1

    {:error, changeset} = Support.create_ticket(org, user, %{title: "", number: 900})
    refute changeset.valid?
    {:ok, second} = Support.create_ticket(org, user, %{"title" => "Second", "number" => "500"})
    assert Map.get(second, :number) == 2
    {:ok, other_first} = Support.create_ticket(other, user, %{title: "Other first"})
    assert Map.get(other_first, :number) == 1

    {:ok, closed} = Support.set_status(first, "closed")
    assert Map.get(closed, :number) == 1
    {:ok, edited} = closed |> Ticket.changeset(%{title: "Edited", body: "Changed", number: 88}) |> Repo.update()
    {:ok, reopened} = Support.set_status(edited, "open")
    assert Map.get(reopened, :number) == 1
    assert Map.get(Support.get_ticket!(second.id), :number) == 2
    {:ok, third} = Support.create_ticket(org, user, %{title: "Third"})
    assert Map.get(third, :number) == 3

    # Highest existing number need not equal the organization's row count.
    raw_insert!(org.id, user.id, 40, "Previously numbered")
    {:ok, next} = Support.create_ticket(org, user, %{title: "After maximum", number: 4})
    assert Map.get(next, :number) == 41
    assert Map.get(Support.get_ticket!(first.id), :number) == 1
    {:ok, other_second} = Support.create_ticket(other, user, %{title: "Other second"})
    assert Map.get(other_second, :number) == 2
  end

  @tag acceptance: "A3"
  test "database rejects duplicate organization-number pairs but permits cross-organization reuse",
       %{user: user, org: org} do
    ticket = ticket_fixture(org, user)
    number = Map.get(ticket, :number)
    assert number == 1

    assert {:error, error} = raw_insert(org.id, user.id, number, "Duplicate")
    assert String.downcase(Exception.message(error)) =~ "unique"
    assert Repo.aggregate(Ticket, :count, :id) == 1

    other = organization_fixture(user)
    assert {:ok, _} = raw_insert(other.id, user.id, number, "Allowed in another org")
    [inserted] = Support.list_tickets(other)
    assert Map.get(inserted, :number) == number
  end

  @tag acceptance: "A4"
  test "titles and export show local numbers while URLs and exported ids retain internal ids",
       %{conn: conn, user: user, org: org} do
    ticket = ticket_fixture(org, user, %{title: "Numbered display", body: "Export body"})
    number = Map.get(ticket, :number)
    assert number == 1
    # Ensure id-based routing is distinguishable from local-number routing.
    if ticket.id == number do
      Ecto.Adapters.SQL.query!(Repo, "UPDATE tickets SET id = ? WHERE id = ?", [ticket.id + 1_000_000, ticket.id])
    end
    [ticket] = Support.list_tickets(org)
    refute ticket.id == number
    path = "/orgs/#{org.slug}/tickets/#{ticket.id}"

    {:ok, _index, html} = live(conn, "/orgs/#{org.slug}/tickets")
    nodes = LazyHTML.from_document(html)
    [link] = nodes |> LazyHTML.query("#tickets a[href='#{path}']") |> Enum.to_list()
    assert normalized_text(link) == "##{number} Numbered display"

    {:ok, _show, show_html} = live(conn, path)
    headers = show_html |> LazyHTML.from_document() |> LazyHTML.query("h1")
    assert Enum.any?(headers, &(normalized_text(&1) == "##{number} Numbered display"))

    payload = conn |> get(path <> "/export.json") |> json_response(200)
    assert payload["number"] == number
    assert payload["id"] == ticket.id
    assert payload["title"] == "Numbered display"
    assert payload["body"] == "Export body"
    assert payload["status"] == "open"
    assert payload["priority"] == "normal"
    assert payload["comments"] == []
  end

  @tag acceptance: "A5"
  test "existing lifecycle and membership presentation remain intact",
       %{conn: conn, user: user, org: org} do
    ticket = ticket_fixture(org, user, %{title: "Keep lifecycle", body: "Keep body"})
    comment_fixture(ticket, user, "Existing comment")
    {:ok, show, html} = live(conn, "/orgs/#{org.slug}/tickets/#{ticket.id}")
    assert html =~ "Keep lifecycle"
    assert html =~ "Keep body"
    assert html =~ "Existing comment"
    show |> form("#comment-form", comment: %{body: "New comment"}) |> render_submit()
    assert render(show) =~ "New comment"
    show |> element("button", "Close ticket") |> render_click()
    assert has_element?(show, "#ticket-status", "closed")
    show |> element("button", "Reopen ticket") |> render_click()
    assert has_element?(show, "#ticket-status", "open")

    viewer_org = organization_fixture()
    {:ok, _membership} = Support.add_member(viewer_org, user, "viewer")
    viewer_ticket = ticket_fixture(viewer_org, user)
    {:ok, index, _html} = live(conn, "/orgs/#{viewer_org.slug}/tickets")
    refute has_element?(index, "#new-ticket-form")
    {:ok, viewer_show, _html} = live(conn, "/orgs/#{viewer_org.slug}/tickets/#{viewer_ticket.id}")
    refute has_element?(viewer_show, "#ticket-actions")
    refute has_element?(viewer_show, "#comment-form")

    unrelated = organization_fixture()
    assert {:error, {:live_redirect, %{to: "/orgs"}}} = live(conn, "/orgs/#{unrelated.slug}/tickets")
  end

  defp normalized_text(node) do
    node |> LazyHTML.text() |> String.replace(~r/\s+/, " ") |> String.trim()
  end

  defp raw_insert(org_id, author_id, number, title) do
    Ecto.Adapters.SQL.query(Repo, """
    INSERT INTO tickets
      (organization_id, author_id, title, status, priority, inserted_at, updated_at, number)
    VALUES (?, ?, ?, 'open', 'normal', '2026-05-01 00:00:00', '2026-05-01 00:00:00', ?)
    """, [org_id, author_id, title, number])
  end

  defp raw_insert!(org_id, author_id, number, title) do
    assert {:ok, result} = raw_insert(org_id, author_id, number, title)
    result
  end
end
