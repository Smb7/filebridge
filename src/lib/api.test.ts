import { describe, expect, it } from "vitest";
import { isAppError } from "./api";

describe("isAppError", () => {
  it("accepts a code and a display message", () => {
    expect(
      isAppError({ code: "auth_failed", message: "Authentication failed" }),
    ).toBe(true);
  });

  it("rejects values that are not a safe error shape", () => {
    expect(isAppError(null)).toBe(false);
    expect(isAppError("auth_failed")).toBe(false);
    expect(isAppError({ code: "auth_failed" })).toBe(false);
    expect(isAppError({ message: "Authentication failed" })).toBe(false);
    expect(isAppError({ code: 1, message: "Authentication failed" })).toBe(
      false,
    );
  });
});
