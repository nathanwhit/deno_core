// deno-lint-ignore-file no-explicit-any
// Copyright 2018-2025 the Deno authors. MIT license.

const { op_test_register } = Deno.core.ops;

/**
 * Define a sync or async test function.
 */
export function test(test: () => void | Promise<void>) {
  op_test_register(test.name, test);
}

/**
 * Assert a value is truthy.
 */
export function assert(value: any, message?: string | undefined) {
  if (!value) {
    const assertion = "Failed assertion" + (message ? `: ${message}` : "");
    console.debug(assertion);
    throw new Error(assertion);
  }
}

export function assertThrows(
  fn: () => void,
  errorClass: any,
  message?: string | undefined,
) {
  try {
    fn();
  } catch (e) {
    if (e instanceof errorClass) {
      if (message) {
        assert(
          e.message.includes(message),
          `Expected message to include ${message}`,
        );
      }
      return;
    }
    throw e;
  }
  throw new Error("Expected function to throw");
}

/**
 * Fails a test.
 */
export function fail(reason: string) {
  console.debug("Failed: " + reason);
  throw new Error("Failed: " + reason);
}

export function equal(a: any, b: any): boolean {
  if (a === b) return true;
  if (typeof a !== typeof b) return false;
  if (Array.isArray(a) && Array.isArray(b)) {
    return a.length === b.length && a.every((v, i) => equal(v, b[i]));
  }
  if (typeof a === "object" && typeof b === "object") {
    const keysA = Object.keys(a);
    const keysB = Object.keys(b);
    if (keysA.length !== keysB.length) return false;
    for (const key of keysA) {
      if (!equal(a[key], b[key])) return false;
    }
    return true;
  }
  return false;
}

/**
 * Assert two values match (==).
 */
export function assertEquals(a1: any, a2: any) {
  assert(equal(a1, a2), `${a1} != ${a2}`);
}

/**
 * Assert two arrays match (==).
 */
export function assertArrayEquals(a1: ArrayLike<any>, a2: ArrayLike<any>) {
  assertEquals(a1.length, a2.length);

  for (const index in a1) {
    assertEquals(a1[index], a2[index]);
  }
}

/**
 * Assert that two stack traces match, minus the line numbers.
 */
export function assertStackTraceEquals(stack1: string, stack2: string) {
  function normalize(s: string) {
    return s.replace(/[ ]+/g, " ")
      .replace(/^ /g, "")
      .replace(/\d+:\d+/g, "line:col")
      .trim();
  }

  assertEquals(normalize(stack1), normalize(stack2));
}
