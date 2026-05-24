/// <reference path="../bun-test.d.ts" />
import { describe, expect, test } from "bun:test";
import { tool } from "@opencode-ai/plugin";
import { coerceOptionalInt, optionalInt } from "../tools/_shared.js";

const z = tool.schema;

describe("optionalInt", () => {
  test("accepts integers in range and undefined", () => {
    const schema = optionalInt(1, 100);
    expect(schema.parse(undefined)).toBeUndefined();
    expect(schema.parse(24)).toBe(24);
    expect(schema.parse(1)).toBe(1);
    expect(schema.parse(100)).toBe(100);
  });

  test("rejects out-of-range and non-integers (Zod-level)", () => {
    const schema = optionalInt(1, 100);
    expect(() => schema.parse(0)).toThrow();
    expect(() => schema.parse(101)).toThrow();
    expect(() => schema.parse(24.5)).toThrow();
    expect(() => schema.parse("24")).toThrow();
    expect(() => schema.parse(null)).toThrow();
  });

  test("MUST be JSON-Schema-representable (regression guard for plugin-load failure)", () => {
    // This test would have caught the v0.30.1 regression where optionalInt
    // used `.transform()` and made every tool's args unconvertible by
    // OpenCode's host Zod, killing plugin load with:
    //   "Transforms cannot be represented in JSON Schema"
    //
    // OpenCode's tool/registry.ts calls `z.toJSONSchema(args, { io: "input" })`
    // on a freshly-wrapped `z.object(args)` at session start. Any throw here
    // crashes plugin load. Keep this guard.
    const wrapped = z.object({ x: optionalInt(1, 60) });
    expect(() => z.toJSONSchema(wrapped, { io: "input" })).not.toThrow();
    const jsonSchema = z.toJSONSchema(wrapped, { io: "input" }) as Record<string, unknown>;
    const props = jsonSchema.properties as Record<string, Record<string, unknown>>;
    // Plain integer schema with bounds — no transform, no anyOf-with-null mess.
    expect(props.x.type).toBe("integer");
    expect(props.x.minimum).toBe(1);
    expect(props.x.maximum).toBe(60);
  });
});

describe("coerceOptionalInt", () => {
  test("treats empty sentinels as undefined", () => {
    expect(coerceOptionalInt(undefined, "x", 1, 100)).toBeUndefined();
    expect(coerceOptionalInt(null, "x", 1, 100)).toBeUndefined();
    expect(coerceOptionalInt("", "x", 1, 100)).toBeUndefined();
    expect(coerceOptionalInt(0, "x", 1, 100)).toBeUndefined();
    expect(coerceOptionalInt(Number.NaN, "x", 1, 100)).toBeUndefined();
  });

  test("accepts numbers and integer strings", () => {
    expect(coerceOptionalInt(24, "x", 1, 100)).toBe(24);
    expect(coerceOptionalInt("24", "x", 1, 100)).toBe(24);
  });

  test("rejects out-of-bounds and non-integers with named errors", () => {
    expect(() => coerceOptionalInt(999, "x", 1, 100)).toThrow("x must be between 1 and 100");
    expect(() => coerceOptionalInt("abc", "x", 1, 100)).toThrow(
      "x must be an integer between 1 and 100",
    );
    expect(() => coerceOptionalInt(24.5, "x", 1, 100)).toThrow(
      "x must be an integer between 1 and 100",
    );
  });
});
