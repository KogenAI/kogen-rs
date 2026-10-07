# Native regression: elixir crates/kogen-core/src/gate/adapters/exunit/ledger_formatter_test.exs
Code.require_file("ledger_formatter.ex", __DIR__)
ExUnit.start()

defmodule KogenLedgerFormatterTest do
  use ExUnit.Case

  setup do
    path =
      Path.join(System.tmp_dir!(), "kogen-ledger-#{System.unique_integer([:positive])}.jsonl")

    System.put_env("KOGEN_LEDGER_REPORT", path)
    on_exit(fn -> File.rm(path) end)
    {:ok, path: path}
  end

  for {task, slug, count} <- [
        {"06", "syn-06-migration-ticket-numbers", 5},
        {"20", "syn-20-email-invite-flow", 6}
      ] do
    @task task
    @slug slug
    @count count
    test "r6 syn-#{task} executed tests retain rows with wrong tags, and record corrected tags",
         %{path: path} do
      source = Path.expand("../../../intent/shaping/testdata/syn-#{@task}-r6_test.exs", __DIR__)
      {:ok, ast} = source |> File.read!() |> Code.string_to_quoted()

      {_, ids} =
        Macro.prewalk(ast, [], fn
          {:@, _, [{:tag, _, [[acceptance: id]]}]} = node, ids -> {node, ids ++ [id]}
          node, ids -> {node, ids}
        end)

      assert length(ids) == @count
      assert {:ok, state} = KogenLedgerFormatter.init([])

      for id <- ids do
        test = %{name: "test #{id}", tags: %{acceptance: id}, state: {:failed, []}}

        assert {:noreply, ^state} =
                 KogenLedgerFormatter.handle_cast({:test_finished, test}, state)
      end

      rows = path |> File.read!() |> String.split("\n", trim: true)
      assert length(rows) == @count
      assert Enum.all?(rows, &String.contains?(&1, ~s("tag":"")))
      assert Enum.all?(rows, &String.contains?(&1, ~s("status":"failed")))
      File.rm!(path)

      for id <- ids do
        status = if id == "A#{@count}", do: nil, else: {:failed, []}
        test = %{name: "test #{id}", tags: %{intent: "#{@slug}/#{id}"}, state: status}
        KogenLedgerFormatter.handle_cast({:test_finished, test}, state)
      end

      rows = path |> File.read!() |> String.split("\n", trim: true)
      assert length(rows) == @count
      assert Enum.count(rows, &String.contains?(&1, ~s("status":"passed"))) == 1

      for {row, id} <- Enum.zip(rows, ids),
          do: assert(String.contains?(row, ~s("tag":"#{@slug}/#{id}")))
    end
  end

  test "module tags and multiple item tags produce their executed rows", %{path: path} do
    {:ok, state} = KogenLedgerFormatter.init([])
    test = %{name: "test shared", tags: %{intent: ["slug/A1", "slug/A2"]}, state: nil}
    KogenLedgerFormatter.handle_cast({:test_finished, test}, state)
    rows = path |> File.read!() |> String.split("\n", trim: true)
    assert length(rows) == 2
    assert Enum.all?(rows, &String.contains?(&1, ~s("status":"passed")))
  end

  test "a compile failure emits no synthetic item rows", %{path: path} do
    {:ok, _state} = KogenLedgerFormatter.init([])

    assert_raise CompileError, fn ->
      Code.compile_string(
        "defmodule KogenMissingFeatureAcceptance do\n  def feature, do: %KogenAbsentFeature{}\nend"
      )
    end

    refute File.exists?(path)
  end

  test "native ExUnit dispatch records inherited module tags and runtime red", %{path: path} do
    script = """
    ExUnit.start(formatters: [KogenLedgerFormatter])
    defmodule NativeTaggedAcceptance do
      use ExUnit.Case
      @moduletag intent: "slug/A1"
      test "missing feature fails at runtime" do
        assert Code.ensure_loaded?(KogenAbsentFeature)
      end
      @tag intent: "slug/A2"
      test "existing behavior stays green" do
        assert true
      end
    end
    """

    {_output, exit_status} =
      System.cmd("elixir", ["-r", Path.join(__DIR__, "ledger_formatter.ex"), "-e", script],
        stderr_to_stdout: true
      )

    assert exit_status == 2
    rows = path |> File.read!() |> String.split("\n", trim: true)
    assert length(rows) == 2

    assert Enum.any?(
             rows,
             &(String.contains?(&1, ~s("tag":"slug/A1")) and
                 String.contains?(&1, ~s("status":"failed")))
           )

    assert Enum.any?(
             rows,
             &(String.contains?(&1, ~s("tag":"slug/A2")) and
                 String.contains?(&1, ~s("status":"passed")))
           )
  end
end
