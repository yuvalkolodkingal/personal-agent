export type Locale = "en-US" | "he-IL";

const english = {
  "nav.Chat": "Chat", "nav.Goals & tasks": "Goals & tasks", "nav.Browser": "Browser",
  "nav.Projects & terminal": "Projects & terminal", "nav.Artifacts": "Artifacts", "nav.History": "History",
  "nav.Memory": "Memory", "nav.Automations": "Automations", "nav.Integrations": "Integrations",
  "nav.Skills & agents": "Skills & agents", "nav.Usage & egress": "Usage & egress",
  "nav.Diagnostics": "Diagnostics", "nav.Settings": "Settings",
  "status.coreOnline": "Core online", "status.privateMode": "Private mode",
  "status.microphone": "Microphone · {state}", "action.language": "Language · English",
} as const;

type MessageKey = keyof typeof english;
const hebrew: Record<MessageKey, string> = {
  "nav.Chat": "שיחה", "nav.Goals & tasks": "מטרות ומשימות", "nav.Browser": "דפדפן",
  "nav.Projects & terminal": "פרויקטים ומסוף", "nav.Artifacts": "תוצרים", "nav.History": "היסטוריה",
  "nav.Memory": "זיכרון", "nav.Automations": "אוטומציות", "nav.Integrations": "שילובים",
  "nav.Skills & agents": "מיומנויות וסוכנים", "nav.Usage & egress": "שימוש ותעבורה",
  "nav.Diagnostics": "אבחון", "nav.Settings": "הגדרות",
  "status.coreOnline": "הליבה פעילה", "status.privateMode": "מצב פרטי",
  "status.microphone": "מיקרופון · {state}", "action.language": "שפה · עברית",
};

const catalogs: Record<Locale, Record<MessageKey, string>> = { "en-US": english, "he-IL": hebrew };

export function translate(locale: Locale, key: MessageKey, values: Record<string, string> = {}): string {
  return catalogs[locale][key].replace(/\{([a-zA-Z0-9_]+)\}/g, (_, name: string) => values[name] ?? `{${name}}`);
}

export function direction(locale: Locale): "ltr" | "rtl" { return locale === "he-IL" ? "rtl" : "ltr"; }
