import { afterEach, describe, expect, it } from "vitest";
import { CATALOGS, interpolate, LOCALES, localeStore, t, withCount } from "../src/i18n";
import { en } from "../src/i18n/messages/en";
import { matchLocale } from "../src/i18n/match";
import { rich } from "../src/i18n/rich";

const placeholders = (s: string) => [...s.matchAll(/\{(\w+)\}/g)].map((m) => m[1]).sort();

describe("catalogs", () => {
  const keys = Object.keys(en).sort();

  it("lists one catalog per language in the menu", () => {
    expect(Object.keys(CATALOGS).sort()).toEqual(LOCALES.map((l) => l.code).sort());
  });

  for (const { code } of LOCALES) {
    describe(code, () => {
      const catalog = CATALOGS[code] as Record<string, string>;

      it("has exactly the English keys", () => {
        expect(Object.keys(catalog).sort()).toEqual(keys);
      });

      it("keeps every placeholder and leaves no message empty", () => {
        for (const key of keys) {
          const message = catalog[key] ?? "";
          expect(message.trim(), key).not.toBe("");
          expect(placeholders(message), key).toEqual(placeholders(en[key as keyof typeof en]));
        }
      });
    });
  }
});

describe("t", () => {
  afterEach(() => localeStore.set("en"));

  it("follows the chosen language", () => {
    expect(t("layout.signOut")).toBe("Sign out");
    localeStore.set("zh-TW");
    expect(t("layout.signOut")).toBe("登出");
    localeStore.set("de");
    expect(t("layout.signOut")).toBe(CATALOGS.de["layout.signOut"]);
  });

  it("fills placeholders", () => {
    expect(t("dead.requeued", { count: 3 })).toBe("Requeued 3");
    expect(withCount("Teams", 2)).toBe("Teams (2)");
    expect(withCount("Teams", undefined)).toBe("Teams");
  });
});

describe("interpolate", () => {
  it("leaves unknown names alone", () => {
    expect(interpolate("{a} and {b}", { a: 1 })).toBe("1 and {b}");
    expect(interpolate("{a}")).toBe("{a}");
  });
});

describe("rich", () => {
  it("puts elements where the sentence names them", () => {
    const parts = rich("before {x} after", { x: "X" }) as Array<{ props: { children: unknown } }>;
    expect(parts.map((p) => p.props.children)).toEqual(["before ", "X", " after"]);
  });
});

describe("matchLocale", () => {
  it("reads zh-CN as simplified and every other Chinese tag as traditional", () => {
    expect(matchLocale(["zh-CN"])).toBe("zh-CN");
    expect(matchLocale(["zh-Hans"])).toBe("zh-CN");
    expect(matchLocale(["zh-Hans-CN"])).toBe("zh-CN");
    expect(matchLocale(["zh-SG"])).toBe("zh-CN");
    expect(matchLocale(["zh"])).toBe("zh-TW");
    expect(matchLocale(["zh-TW"])).toBe("zh-TW");
    expect(matchLocale(["zh-HK"])).toBe("zh-TW");
    expect(matchLocale(["zh-Hant-HK"])).toBe("zh-TW");
  });

  it("falls back from a regional variant to its language", () => {
    expect(matchLocale(["fr-CA"])).toBe("fr");
    expect(matchLocale(["de-AT"])).toBe("de");
    expect(matchLocale(["es-MX"])).toBe("es");
    expect(matchLocale(["vi-VN"])).toBe("vi");
    expect(matchLocale(["en-GB"])).toBe("en");
  });

  it("takes the first preference it ships, else English", () => {
    expect(matchLocale(["pt-BR", "ja", "de-DE", "fr"])).toBe("de");
    expect(matchLocale(["pt-BR", "ja"])).toBe("en");
    expect(matchLocale([])).toBe("en");
  });
});
