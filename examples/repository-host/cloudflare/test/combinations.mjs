// Preserve nested-loop order while keeping each matrix workload in one scope.
export function* combinations(...axes) {
  if (!axes.length) { yield []; return; }
  const [first, ...rest] = axes;
  for (const value of first) for (const tail of combinations(...rest)) yield [value, ...tail];
}
