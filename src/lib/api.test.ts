import { describe, expect, it } from "vitest";
import { blankToNull, isAppError, keyPathForMethod } from "./api";

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

describe("connection form helpers", () => {
  it("sends a key path only for key file authentication", () => {
    expect(keyPathForMethod("key_file", " /home/ubuntu/.ssh/id_ed25519 ")).toBe(
      "/home/ubuntu/.ssh/id_ed25519",
    );
    expect(keyPathForMethod("password", "/home/ubuntu/.ssh/id_ed25519")).toBe(
      null,
    );
    expect(keyPathForMethod("key_file", "  ")).toBe(null);
  });

  it("turns blank optional fields into null", () => {
    expect(blankToNull("  ")).toBe(null);
    expect(blankToNull(" /etc/telegraf ")).toBe("/etc/telegraf");
  });
});
