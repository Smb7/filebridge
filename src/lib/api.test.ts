import { describe, expect, it } from "vitest";
import {
  blankToNull,
  hostKeyPromptCopy,
  isAppError,
  keyPathForMethod,
  secretFromFields,
} from "./api";

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

  it("does not send a secret unless the user typed one", () => {
    expect(secretFromFields("password", "  ", "", "")).toBe(null);
    expect(secretFromFields("agent", "hunter2", "key", "pass")).toBe(null);
    expect(secretFromFields("password", "hunter2", "ignored", "")).toEqual({
      password: "hunter2",
    });
  });
});

describe("host key prompt", () => {
  it("uses a louder replace action when the key changed", () => {
    expect(hostKeyPromptCopy("unknown").confirm).toBe("Trust this key");
    expect(hostKeyPromptCopy("changed").title).toBe("Host key changed");
    expect(hostKeyPromptCopy("changed").confirm).toBe("Replace trusted key");
    expect(hostKeyPromptCopy("changed").confirm).not.toBe(
      hostKeyPromptCopy("unknown").confirm,
    );
  });
});
