/**
 * Program preparation on the host: strip TypeScript types, then place the
 * program in a strict async function whose parameters are the declared
 * binding namespaces and error classes, behind the binding prelude that runs
 * inside the QuickJS VM.
 * @module @deepseek-ai/dsh-ptc-runtime-codemode/program
 */

import { stripTypeScriptTypes } from 'node:module'
import type { PtcBindingNamespace } from '@deepseek-ai/dsh-ptc-runtime'
import { CODEMODE_SCRIPT_PREFIX } from './protocol.ts'
import type { ProgramLayout } from './protocol.ts'

/** The one pi-codemode global the prelude calls; every binding call crosses it as `[namespace, member, args]`. */
export const CALL_GLOBAL = '__dsh_call__'

/** Prelude entry point, also a seam-reserved binding global. */
const MAIN = '__dsh_main__'

/**
 * Names pi-codemode defines for its own script contract. The program does not
 * see them unless a binding declares the same name: its namespace is the
 * declared bindings, `console`, and the standard built-ins.
 */
const HIDDEN_NAMES = [MAIN, CALL_GLOBAL, 'tools', 'ALL_TOOLS', 'text', 'image', 'exit', 'store', 'load']

// Positions survive stripping because types become whitespace. The newline
// keeps program line numbers starting at 1 after slicing the prefix off.
const STRIP_PREFIX = 'async function __dsh_program__() {\n'
const STRIP_SUFFIX = '\n}'

/** Line terminators as the QuickJS tokenizer counts them. */
const LINE_TERMINATOR = /\r\n?|[\n\u2028\u2029]/g

/**
 * Prelude evaluated inside the VM ahead of the program body. It runs before
 * the program, so the intrinsics it captures are the original ones; a program
 * that later corrupts built-ins can only break its own calls, because the host
 * validates every call and completion again.
 *
 * Each namespace member snapshots its arguments under the lossless-JSON rules,
 * sends `[namespace index, member, args]` through the call global, and turns
 * a host rejection into the namespace's error class. Reading a missing member
 * throws a TypeError that names close matches. The completion crosses as
 * `[0, value]`, or `[1]` when it is not lossless JSON.
 */
const PRELUDE = `async function ${MAIN}(body, declared) {
  "use strict";
  const call = globalThis.${CALL_GLOBAL};
  const ErrorCtor = Error;
  const TypeErrorCtor = TypeError;
  const ProxyCtor = Proxy;
  const SetCtor = Set;
  const objectPrototype = Object.prototype;
  const arrayPrototype = Array.prototype;
  const create = Object.create;
  const defineProperty = Object.defineProperty;
  const freeze = Object.freeze;
  const getPrototypeOf = Object.getPrototypeOf;
  const hasOwn = Object.hasOwn;
  const is = Object.is;
  const isArray = Array.isArray;
  const isFinite = Number.isFinite;
  const ownKeys = Reflect.ownKeys;
  const reflectGet = Reflect.get;
  const apply = Reflect.apply;
  const setAdd = Set.prototype.add;
  const setDelete = Set.prototype.delete;
  const setHas = Set.prototype.has;
  const propertyIsEnumerable = objectPrototype.propertyIsEnumerable;
  const LOSSY = freeze(create(null));

  function data(value, enumerable, writable) {
    const descriptor = create(null);
    descriptor.value = value;
    descriptor.enumerable = enumerable;
    descriptor.writable = writable;
    descriptor.configurable = writable;
    return descriptor;
  }

  function copy(value, ancestors) {
    if (value === null || typeof value === "boolean" || typeof value === "string") return value;
    if (typeof value === "number") {
      if (!isFinite(value) || is(value, -0)) throw LOSSY;
      return value;
    }
    if (typeof value !== "object" || apply(setHas, ancestors, [value])) throw LOSSY;
    const keys = ownKeys(value);
    if (isArray(value)) {
      if (getPrototypeOf(value) !== arrayPrototype || keys.length !== value.length + 1) throw LOSSY;
      const result = [];
      apply(setAdd, ancestors, [value]);
      for (let index = 0; index < value.length; index++) {
        if (!hasOwn(value, index)) throw LOSSY;
        defineProperty(result, index, data(copy(value[index], ancestors), true, true));
      }
      apply(setDelete, ancestors, [value]);
      return result;
    }
    const prototype = getPrototypeOf(value);
    if (prototype !== objectPrototype && prototype !== null) throw LOSSY;
    const result = create(null);
    apply(setAdd, ancestors, [value]);
    for (let index = 0; index < keys.length; index++) {
      const key = keys[index];
      if (typeof key !== "string" || !apply(propertyIsEnumerable, value, [key])) throw LOSSY;
      defineProperty(result, key, data(copy(value[key], ancestors), true, true));
    }
    apply(setDelete, ancestors, [value]);
    return result;
  }

  function snapshot(value) {
    try {
      return copy(value, new SetCtor());
    } catch {
      return LOSSY;
    }
  }

  function makeErrorClass(name, member) {
    const BindingError = class extends ErrorCtor {
      constructor(memberName, message) {
        super(message);
        defineProperty(this, "name", data(name, true, false));
        defineProperty(this, member, data(memberName, true, false));
      }
    };
    defineProperty(BindingError, "name", data(name, false, false));
    return BindingError;
  }

  function access(label, name) {
    return /^[A-Za-z_$][A-Za-z0-9_$]*$/.test(name) ? label + "." + name : label + "[" + JSON.stringify(name) + "]";
  }

  function comparable(name) {
    return name.toLowerCase().replace(/[^a-z0-9]/g, "");
  }

  function missing(label, property, names) {
    const wanted = comparable(property);
    const exact = names.filter((name) => comparable(name) === wanted);
    const close = exact.length > 0 ? exact : names.filter((name) => {
      const candidate = comparable(name);
      return wanted !== "" && candidate !== "" && (candidate.includes(wanted) || wanted.includes(candidate));
    });
    let message = access(label, property) + " does not exist.";
    if (close.length > 0) message += " Did you mean " + close.slice(0, 5).map((name) => access(label, name)).join(", ") + "?";
    else if (names.length <= 20) message += " Available: " + names.map((name) => access(label, name)).join(", ") + ".";
    return message + ' Test membership with ' + JSON.stringify(property) + " in " + label + ".";
  }

  function guard(target, label, names) {
    return new ProxyCtor(target, {
      get(object, property, receiver) {
        if (typeof property !== "string" || hasOwn(object, property) || property in objectPrototype || property === "then" || property === "toJSON") {
          return reflectGet(object, property, receiver);
        }
        throw new TypeErrorCtor(missing(label, property, names));
      },
    });
  }

  const namespaces = [];
  const errorClasses = [];
  for (let index = 0; index < declared.length; index++) {
    const namespace = declared[index];
    const ErrorClass = namespace.errorClass === undefined ? undefined : makeErrorClass(namespace.errorClass.name, namespace.errorClass.member);
    const failure = (member, message) => ErrorClass === undefined ? new ErrorCtor(message) : new ErrorClass(member, message);
    const members = create(null);
    for (const member of namespace.names) {
      defineProperty(members, member, data(async (args) => {
        const detached = snapshot(args);
        if (detached === LOSSY) throw failure(member, "binding arguments must be lossless JSON");
        try {
          return await call([index, member, detached]);
        } catch (error) {
          throw failure(member, error instanceof ErrorCtor ? error.message : String(error));
        }
      }, true, false));
    }
    namespaces.push(guard(freeze(members), namespace.global, namespace.names));
    if (ErrorClass !== undefined) errorClasses.push(ErrorClass);
  }

  const value = await body(...namespaces, ...errorClasses)();
  if (value === undefined) return undefined;
  const detached = snapshot(value);
  return detached === LOSSY ? [1] : [0, detached];
}`

/** Source for pi-codemode and the program's place in it. */
export interface PreparedProgram {
  /** Script body pi-codemode evaluates, before the worker header is added. */
  readonly code: string
  /** Where the program body sits in the evaluated script. */
  readonly layout: ProgramLayout
}

/**
 * Strip a program's types and wrap it for the VM.
 * @param program - the model-written TypeScript function body.
 * @param bindings - validated namespaces, in declaration order.
 * @returns the pi-codemode script and the program layout inside it.
 * @throws The stripper's diagnostic when the program is not valid erasable TypeScript.
 */
export function prepareProgram(program: string, bindings: readonly PtcBindingNamespace[]): PreparedProgram {
  const stripped = stripTypeScriptTypes(STRIP_PREFIX + program + STRIP_SUFFIX)
  const body = stripped.slice(STRIP_PREFIX.length, stripped.length - STRIP_SUFFIX.length)
  const declared = bindings.map(binding => ({
    global: binding.global,
    names: Object.keys(binding.functions),
    ...binding.errorClass === undefined
      ? {}
      : { errorClass: { name: binding.errorClass.name, member: binding.errorClass.memberNameProperty } },
  }))
  const visible = [
    ...bindings.map(binding => binding.global),
    ...bindings.flatMap(binding => binding.errorClass === undefined ? [] : [binding.errorClass.name]),
  ]
  const parameters = [...visible, ...HIDDEN_NAMES.filter(name => !visible.includes(name))]
  const prefix = `return await ${MAIN}((${parameters.join(',')})=>async function(){"use strict";`
  // A JSON text is a JavaScript expression once the two line separators JSON leaves raw are escaped.
  const literal = JSON.stringify(declared).replaceAll('\u2028', '\\u2028').replaceAll('\u2029', '\\u2029')
  return {
    code: `${prefix}${body}\n},${literal});\n${PRELUDE}`,
    layout: {
      lines: body.split(LINE_TERMINATOR).length,
      column: CODEMODE_SCRIPT_PREFIX.length + prefix.length + 1,
    },
  }
}
