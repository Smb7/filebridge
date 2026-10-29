import { describe, expect, it } from "vitest";
import {
  blankToNull,
  hostKeyPromptCopy,
  isAppError,
  keyPathForMethod,
  planCredentialWrite,
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
    expect(secretFromFields("private_key", "", "ignored-key", "")).toEqual({
      privateKeyPem: "ignored-key",
    });
  });
});

describe("planCredentialWrite", () => {
  it("refuses secret changes while the vault is locked", () => {
    expect(
      planCredentialWrite({
        locked: true,
        methodChanged: false,
        authMethod: "password",
        hasSecret: true,
      }),
    ).toEqual({
      action: "refuse",
      message: "Unlock the vault before saving a password or key.",
    });
    expect(
      planCredentialWrite({
        locked: true,
        methodChanged: true,
        authMethod: "agent",
        hasSecret: false,
      }).action,
    ).toBe("refuse");
  });

  it("clears a secret when authentication no longer uses one", () => {
    expect(
      planCredentialWrite({
        locked: false,
        methodChanged: true,
        authMethod: "agent",
        hasSecret: false,
      }),
    ).toEqual({ action: "clear" });
    expect(
      planCredentialWrite({
        locked: false,
        methodChanged: false,
        authMethod: "password",
        hasSecret: false,
      }),
    ).toEqual({ action: "keep" });
    expect(
      planCredentialWrite({
        locked: false,
        methodChanged: false,
        authMethod: "password",
        hasSecret: true,
      }),
    ).toEqual({ action: "set" });
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
