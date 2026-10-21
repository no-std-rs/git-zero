// SQLite adapter for focused Store tests; runtime tests use actual bindings.
export function sqliteContext(database) {
  return { storage: {
    sql: { exec(statement, ...bindings) {
      const rows = database.prepare(statement).all(...bindings.map((value) => value instanceof ArrayBuffer ? new Uint8Array(value) : value));
      return { toArray: () => rows };
    } },
    transactionSync(operation) {
      database.exec("BEGIN");
      try { const result = operation(); database.exec("COMMIT"); return result; }
      catch (error) { database.exec("ROLLBACK"); throw error; }
    },
    async setAlarm() {},
  } };
}
