import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import App from "./App";
import { ConfirmDialog } from "./components";
import { initialState, reducer } from "./state";

afterEach(cleanup);

describe("reducer", () => {
  it("verfolgt Phasen, Tool-Aufrufe, Ergebnisse und Prüfungen", () => {
    let s = reducer(initialState, { type: "user", text: "Lösche alt.txt" });
    expect(s.reactor).toBe("thinking");
    s = reducer(s, { type: "agent", event: { type: "phase", phase: "Execute" } });
    s = reducer(s, { type: "agent", event: { type: "phase", phase: "Execute" } });
    expect(s.phase).toBe("Execute");
    expect(s.activity.map((a) => a.kind)).toEqual(["run"]);
    s = reducer(s, { type: "agent", event: { type: "tool_call", tool: "fs_trash", args: { path: "alt.txt" } } });
    expect(s.reactor).toBe("acting");
    s = reducer(s, { type: "agent", event: { type: "tool_result", tool: "fs_trash", status: "NotConfirmed", summary: "abgelehnt" } });
    s = reducer(s, { type: "agent", event: { type: "verified", tool: "fs_trash", ok: false, detail: "existiert noch" } });
    const call = s.activity.find((a) => a.kind === "call");
    expect(call).toMatchObject({ status: "NotConfirmed", summary: "abgelehnt", verified: { ok: false } });
    s = reducer(s, { type: "reply", text: "Nichts gelöscht.", meta: "" });
    expect(s.reactor).toBe("idle");
    expect(s.messages.map((m) => m.role)).toEqual(["user", "jarvis"]);
  });
});

describe("ConfirmDialog", () => {
  const req = {
    id: "1",
    tool: "fs_trash",
    description: "Papierkorb",
    access: "Destructive",
    risk: "High",
    reason: "<img src=x onerror=alert(1)> alt.txt in den Papierkorb legen",
    paths: ["/Users/du/alt.txt"],
  };

  it("zeigt Inhalte nur als Text und fokussiert 'Ablehnen'", () => {
    const onAnswer = vi.fn();
    const { container } = render(<ConfirmDialog req={req} onAnswer={onAnswer} />);
    expect(container.querySelector("img")).toBeNull();
    expect(screen.getByText(/onerror=alert/)).toBeTruthy();
    expect(document.activeElement?.textContent).toBe("Ablehnen");
    fireEvent.keyDown(window, { key: "Escape" });
    expect(onAnswer).toHaveBeenCalledWith(false);
    fireEvent.click(screen.getByText("Ja, ausführen"));
    expect(onAnswer).toHaveBeenLastCalledWith(true);
  });
});

describe("Push-to-Talk", () => {
  it("nimmt nur während des Haltens auf und schickt das Transkript an den Agenten", async () => {
    render(<App />);
    const mic = await screen.findByLabelText("Sprechtaste (gedrückt halten)");
    await waitFor(() => expect((mic as HTMLButtonElement).disabled).toBe(false));
    await act(async () => fireEvent.pointerDown(mic));
    expect(mic.getAttribute("aria-pressed")).toBe("true");
    expect(screen.getByRole("status").getAttribute("aria-label")).toContain("Hört zu");
    await act(async () => fireEvent.pointerUp(mic));
    await waitFor(() => expect(document.querySelector(".msg--user p")?.textContent).toBe("Finde meine PDFs im Ordner Dokumente"));
    await waitFor(() => expect(screen.getByText(/3 PDFs gefunden/)).toBeTruthy(), { timeout: 3000 });
  });
});

describe("App mit Mock-Backend", () => {
  it("führt eine destruktive Anfrage nur nach Bestätigung aus", async () => {
    render(<App />);
    const input = await screen.findByLabelText("Nachricht an JARVIS");
    fireEvent.change(input, { target: { value: "Lösche die alte Rechnung" } });
    fireEvent.keyDown(input, { key: "Enter" });
    const dialog = await screen.findByRole("alertdialog", {}, { timeout: 3000 });
    expect(dialog.textContent).toContain("fs_trash");
    await act(async () => fireEvent.click(screen.getByText("Ablehnen")));
    await waitFor(() => expect(screen.getByText(/ich habe nichts gelöscht/)).toBeTruthy(), { timeout: 3000 });
    expect(screen.queryByRole("alertdialog")).toBeNull();
    expect(screen.getAllByText(/fs_trash/).length).toBeGreaterThan(0);
  });
});
