import { describe, expect, it } from "bun:test";
import { direction, translate } from "./i18n";

describe("localization", () => {
  it("has complete English and Hebrew navigation with interpolation", () => {
    expect(translate("en-US", "nav.Memory")).toBe("Memory");
    expect(translate("he-IL", "nav.Memory")).toBe("זיכרון");
    expect(translate("he-IL", "status.microphone", { state: "כבוי" })).toBe("מיקרופון · כבוי");
    expect(direction("he-IL")).toBe("rtl");
  });
});
