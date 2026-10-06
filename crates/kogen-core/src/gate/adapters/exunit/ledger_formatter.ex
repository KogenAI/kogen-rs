defmodule KogenLedgerFormatter do
  def init(_opts), do: {:ok, nil}

  def handle_cast({:test_finished, test}, state) do
    tags = Map.get(test, :tags, %{})
    tag = Map.get(tags, :intent) || Map.get(tags, "intent")

    if is_binary(tag) do
      name = test |> Map.get(:name, "") |> to_string() |> String.trim_leading("test ")
      status = test |> Map.get(:state) |> status()
      row = ~s({"tag":"#{escape(tag)}","test":"#{escape(name)}","status":"#{status}"}) <> "\n"
      File.write!(System.fetch_env!("KOGEN_LEDGER_REPORT"), row, [:append])
    end

    {:noreply, state}
  end

  def handle_cast(_event, state), do: {:noreply, state}

  defp status(nil), do: "passed"
  defp status({:failed, _}), do: "failed"
  defp status({:skipped, _}), do: "skipped"
  defp status({:excluded, _}), do: "excluded"
  defp status({:invalid, _}), do: "invalid"
  defp status(:failed), do: "failed"
  defp status(:skipped), do: "skipped"
  defp status(:excluded), do: "excluded"
  defp status(_), do: "invalid"

  defp escape(value) do
    value
    |> String.to_charlist()
    |> Enum.map_join(fn
      ?\\ -> "\\\\"
      ?" -> "\\\""
      ?\n -> "\\n"
      ?\r -> "\\r"
      ?\t -> "\\t"
      code when code < 0x20 ->
        "\\u" <> (code |> Integer.to_string(16) |> String.pad_leading(4, "0"))
      code -> <<code::utf8>>
    end)
  end
end
