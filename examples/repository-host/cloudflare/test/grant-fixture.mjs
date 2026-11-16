import { createHmac } from "node:crypto";
// Public fixture data, never a deployment key. Independent Node minting oracle.
export const GRANT_KEY = "0123456789abcdef0123456789abcdef";
export function mint(namespace, access = "read", { expiry = Math.floor(Date.now() / 1000) + 120, actor = "fixture-user", secret = GRANT_KEY } = {}) {
  const label = Buffer.from(actor).toString("base64url");
  const message = `git-zero-grant-v1\n${namespace}\n${access}\n${expiry}\n${label}\n`;
  const mac = createHmac("sha256", secret).update(message).digest("base64url");
  return `v1.${namespace}.${access}.${expiry}.${label}.${mac}`;
}
