// Окружение скрипта crabtrap: console (записи забирает рантайм после
// каждого вызова) и проверка значений перед переводом в JSON на стороне
// Rust (там обход рекурсивный: слишком глубокое значение уронило бы стек).
// Выполняется один раз на JS-контекст потока пула.
(function (global) {
  const MAX_ENTRIES = 200;
  const MAX_LENGTH = 16384;
  const LIMITS = { depth: 64, nodes: 200000, chars: 64 * 1024 * 1024 };

  // Итеративный обход: глубина, число узлов (циклы упираются в него),
  // суммарная длина строк.
  const check = (root) => {
    const stack = [[root, 0]];
    let nodes = 0;
    let chars = 0;
    while (stack.length > 0) {
      const [value, depth] = stack.pop();
      if (++nodes > LIMITS.nodes) throw new RangeError("value is too large (too many items)");
      if (typeof value === "string") {
        chars += value.length;
        if (chars > LIMITS.chars) throw new RangeError("value is too large (strings over 64 MiB)");
        continue;
      }
      if (value === null || typeof value !== "object") continue;
      if (depth >= LIMITS.depth) throw new RangeError("value is nested too deeply");
      if (Array.isArray(value)) {
        if (value.length > LIMITS.nodes) throw new RangeError("value is too large (array too long)");
        for (let i = 0; i < value.length; i++) stack.push([value[i], depth + 1]);
      } else {
        for (const key of Object.keys(value)) {
          chars += key.length;
          stack.push([value[key], depth + 1]);
        }
      }
    }
    return true;
  };
  const stringify = (value, indent) => {
    check(value);
    return JSON.stringify(value, null, indent);
  };
  Object.defineProperty(global, "__crabtrapCheck", { value: check });
  // Тело-объект сериализуется средствами JS: целые остаются целыми.
  Object.defineProperty(global, "__crabtrapStringify", { value: (value) => stringify(value) });

  const format = (value) => {
    if (typeof value === "string") return value;
    if (value === undefined) return "undefined";
    if (typeof value === "function") return "[Function]";
    if (value instanceof Error) return value.stack || String(value);
    try {
      const text = stringify(value, 2);
      return text === undefined ? String(value) : text;
    } catch (_) {
      return String(value);
    }
  };
  const logs = [];
  const push = (level) => (...args) => {
    if (logs.length >= MAX_ENTRIES) return;
    let text = args.map(format).join(" ");
    if (text.length > MAX_LENGTH) text = text.slice(0, MAX_LENGTH) + "…";
    logs.push([level, text]);
  };
  global.console = {
    log: push("log"),
    info: push("info"),
    warn: push("warn"),
    error: push("error"),
    debug: push("debug"),
  };
  Object.defineProperty(global, "__crabtrapTakeLogs", {
    value: () => logs.splice(0, logs.length),
  });
})(globalThis);
