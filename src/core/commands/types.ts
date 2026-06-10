export type CommandHandler = (args?: unknown) => void | Promise<void>;

/**
 * Ein Command ist die zentrale Einheit für alles Ausführbare in Editron.
 * Jede Aktion — Menü, Button, Shortcut, Command Palette — läuft über
 * die Registry, damit Shortcuts für ausnahmslos alles belegbar sind.
 */
export interface Command {
  /** Eindeutige ID, z. B. "playback.togglePlay" */
  id: string;
  /** Nutzersichtbarer Titel (deutsch), z. B. "Wiedergabe starten/stoppen" */
  title: string;
  /** Kategorie für Command Palette und Shortcut-Editor, z. B. "Wiedergabe" */
  category: string;
  description?: string;
  /**
   * Kontext-Ausdruck (when-Klausel), der bestimmt, wann der Command
   * ausführbar ist. Syntax siehe evaluateWhen() in context.ts.
   * undefined = immer ausführbar.
   */
  when?: string;
  /**
   * Erlaubt das Auslösen per Key-Auto-Repeat (gehaltene Taste).
   * Default false — nur für wiederholbare Aktionen wie Frame-Steps
   * oder Zoom setzen, nie für Toggles/Shuttle.
   */
  allowRepeat?: boolean;
  run: CommandHandler;
}
