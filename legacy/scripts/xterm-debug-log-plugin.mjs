import ts from "typescript";

const DEBUG_LABELS = ["disposed?", "size?", "arr?"];
const LISTENER_ERROR = "Attempted to dispose unknown listener";

function isDebugLog(node, label) {
  return (
    ts.isCallExpression(node) &&
    ts.isPropertyAccessExpression(node.expression) &&
    ts.isIdentifier(node.expression.expression) &&
    node.expression.expression.text === "console" &&
    node.expression.name.text === "log" &&
    node.arguments[0] &&
    ts.isStringLiteral(node.arguments[0]) &&
    node.arguments[0].text === label
  );
}

function debugContextCall(logs, sourceFile, source) {
  // One console.error per anomalous throw, carrying the same context dev prints: the labels plus the emitter
  // state (`_disposed`, `_size`, `JSON.stringify(_listeners)`). Arguments are still evaluated in order, with
  // their side effects and failures, and the emitted throw is unchanged. `_listeners` only holds emitter
  // records `{ value: function, id: number, stack?: string }`, so no terminal payload is serialized here.
  const args = logs.flatMap((call) =>
    call.arguments.map((argument) => source.slice(argument.getStart(sourceFile), argument.end)),
  );
  const evaluation = logs.some((call) => call.arguments.some(ts.isSpreadElement))
    ? `[${args.join(", ")}]`
    : args.join(", ");
  return `console.error(${evaluation})`;
}

function commaExpressions(node) {
  return ts.isBinaryExpression(node) && node.operatorToken.kind === ts.SyntaxKind.CommaToken
    ? [...commaExpressions(node.left), ...commaExpressions(node.right)]
    : [node];
}

function isListenerError(node) {
  return (
    ts.isNewExpression(node) &&
    ts.isIdentifier(node.expression) &&
    node.expression.text === "Error" &&
    node.arguments?.length === 1 &&
    ts.isStringLiteral(node.arguments[0]) &&
    node.arguments[0].text === LISTENER_ERROR
  );
}

function listenerErrorLogs(node) {
  if (!ts.isThrowStatement(node) || !node.expression) return null;
  const expressions = commaExpressions(node.expression);
  return expressions.length === DEBUG_LABELS.length + 1 &&
    DEBUG_LABELS.every((label, index) => isDebugLog(expressions[index], label)) &&
    isListenerError(expressions.at(-1))
    ? expressions.slice(0, -1)
    : null;
}

export function suppressXtermDebugLogs(source) {
  const sourceFile = ts.createSourceFile("xterm.mjs", source, ts.ScriptTarget.Latest, true, ts.ScriptKind.JS);
  const replacements = [];

  function visit(node) {
    const logs = listenerErrorLogs(node);
    if (logs) {
      replacements.push({
        start: logs[0].getStart(sourceFile),
        end: logs.at(-1).end,
        text: debugContextCall(logs, sourceFile, source),
      });
      return;
    }
    ts.forEachChild(node, visit);
  }

  visit(sourceFile);
  if (!replacements.length) return null;

  return replacements
    .sort((left, right) => right.start - left.start)
    .reduce((result, { start, end, text }) => result.slice(0, start) + text + result.slice(end), source);
}

export function xtermDebugLogPlugin() {
  return {
    name: "xterm-debug-log-suppression",
    apply: "build",
    transform(source, id) {
      const path = id.replaceAll("\\", "/").split("?", 1)[0];
      const allowed = [
        "/node_modules/@xterm/xterm/lib/xterm.mjs",
        "/node_modules/@xterm/addon-webgl/lib/addon-webgl.mjs",
        "/node_modules/@xterm/addon-unicode11/lib/addon-unicode11.mjs",
      ];
      if (!allowed.some((suffix) => path.endsWith(suffix))) {
        return null;
      }
      const code = suppressXtermDebugLogs(source);
      return code === null ? null : { code, map: null };
    },
  };
}
