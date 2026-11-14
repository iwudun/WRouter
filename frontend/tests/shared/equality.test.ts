import { describe, expect, it } from "vitest";
import { deepEqual } from "@/pages/home/shared/equality";

/**
 * Polling-snapshot equality short-circuit: snapshots come from IPC as plain
 * JSON (no functions / circular references), so a compact recursive compare is
 * all we need. Same-value primitives and structurally equal nested objects /
 * arrays must compare equal; anything differing by key, value, length or
 * literal identity must not.
 */

describe("deepEqual primitives", () => {
  it("treats strictly equal values as equal, including same reference", () => {
    const obj = { a: 1 };
    expect(deepEqual(obj, obj)).toBe(true);
    const arr = [1, 2];
    expect(deepEqual(arr, arr)).toBe(true);
  });

  it("treats equal primitives as equal", () => {
    expect(deepEqual("a", "a")).toBe(true);
    expect(deepEqual(1, 1)).toBe(true);
    expect(deepEqual(true, true)).toBe(true);
  });

  it("distinguishes null, undefined, 0 and empty string from each other", () => {
    expect(deepEqual(null, undefined)).toBe(false);
    expect(deepEqual(null, 0)).toBe(false);
    expect(deepEqual(null, "")).toBe(false);
    expect(deepEqual(undefined, 0)).toBe(false);
    expect(deepEqual(undefined, "")).toBe(false);
    expect(deepEqual(0, "")).toBe(false);
  });

  it("returns false for differing primitives", () => {
    expect(deepEqual("a", "b")).toBe(false);
    expect(deepEqual(1, 2)).toBe(false);
    expect(deepEqual(true, false)).toBe(false);
  });

  it("returns false for NaN by === semantics (documented behavior)", () => {
    // NaN !== NaN under strict equality (Array.indexOf uses === under the
    // hood), so deepEqual must also report NaN as unequal to itself.
    expect([NaN].indexOf(NaN)).toBe(-1);
    expect(deepEqual(NaN, NaN)).toBe(false);
  });
});

describe("deepEqual objects", () => {
  it("compares equal when keys and values match", () => {
    expect(deepEqual({ a: 1, b: "x", c: true }, { a: 1, b: "x", c: true })).toBe(true);
  });

  it("compares unequal when one object has an extra key", () => {
    expect(deepEqual({ a: 1 }, { a: 1, b: 2 })).toBe(false);
  });

  it("compares unequal when one object is missing a key", () => {
    expect(deepEqual({ a: 1, b: 2 }, { a: 1 })).toBe(false);
  });

  it("compares unequal when a value differs", () => {
    expect(deepEqual({ a: 1 }, { a: 2 })).toBe(false);
  });

  it("compares unequal when the other object lacks the key (hasOwnProperty check)", () => {
    const b = Object.create({ a: 1 });
    expect(deepEqual({ a: 1 }, b)).toBe(false);
  });
});

describe("deepEqual arrays", () => {
  it("compares equal for identical contents", () => {
    expect(deepEqual([1, 2, 3], [1, 2, 3])).toBe(true);
    expect(deepEqual([], [])).toBe(true);
  });

  it("compares unequal for different lengths", () => {
    expect(deepEqual([1, 2], [1, 2, 3])).toBe(false);
  });

  it("compares unequal when nested contents differ", () => {
    expect(deepEqual([{ a: 1 }, [2]], [{ a: 1 }, [3]])).toBe(false);
  });

  it("compares an array unequal to a non-array object", () => {
    expect(deepEqual([1], { 0: 1, length: 1 })).toBe(false);
  });
});

describe("deepEqual nested structures", () => {
  const nested = {
    rows: [
      { id: "r1", values: [1, 2, { label: "x", tags: ["a", "b"] }] },
      { id: "r2", values: [3, 4, { label: "y", tags: [] }] }
    ],
    meta: { total: 2, flag: true }
  };

  it("compares equal for deep object > array > object structures", () => {
    expect(deepEqual(nested, JSON.parse(JSON.stringify(nested)))).toBe(true);
  });

  it("compares unequal when only a leaf value differs", () => {
    const mutated = JSON.parse(JSON.stringify(nested));
    mutated.rows[0].values[2].tags[1] = "z";
    expect(deepEqual(nested, mutated)).toBe(false);
  });
});
