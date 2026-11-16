// SQLite adapter for focused Store tests; runtime tests use actual bindings.
export function sqliteContext(database) {
  const kv = new Map();
  return { storage: {
    sql: { exec(statement, ...bindings) {
      // node:sqlite binds a zero-length ArrayBuffer view with a null native
      // pointer as SQL NULL. Keep backing storage for the actual empty BLOB.
      const rows = database.prepare(statement).all(...bindings.map((value) => value instanceof ArrayBuffer ? (value.byteLength ? new Uint8Array(value) : new Uint8Array(1).subarray(0, 0)) : value));
      return { toArray: () => rows };
    } },
    transactionSync(operation) {
      database.exec("BEGIN");
      try { const result = operation(); database.exec("COMMIT"); return result; }
      catch (error) { database.exec("ROLLBACK"); throw error; }
    },
    async get(key) { return kv.has(key) ? structuredClone(kv.get(key)) : undefined; },
    async put(key, value) { kv.set(key, structuredClone(value)); },
    async deleteAlarm() {},
    async deleteAll() {
      for (const row of database.prepare("SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%'").all()) database.exec(`DROP TABLE "${row.name.replaceAll('"', '""')}"`);
      kv.clear();
    },
    async setAlarm() {},
  } };
}
