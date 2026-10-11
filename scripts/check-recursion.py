#!/usr/bin/env python3
"""No runtime recursion (CLAUDE.md §1): call-graph cycles through the workspace's own code, found
on the built code.

    python3 scripts/check-recursion.py [--target-dir DIR] [--list]
    python3 scripts/check-recursion.py --self-test

The code read is LLVM IR compiled at opt-level 0. No call is inlined there but an always-inline
one, and inlining a call of a cycle leaves the cycle shorter, never gone; no tail call is made a
jump. One build, `cargo build --workspace --lib --bins --test '*' --all-features --locked`, emits
the IR beside each crate it compiles (RUSTFLAGS gains `--emit=llvm-ir`, which rustc adds to the
outputs cargo asks for), with one codegen unit per crate, no debug info and no incremental reuse,
into a target directory of its own (target/recursion by default). The profile comes from the
environment, so the tree is not touched. The directory serves one tree: it records the tree's root
and refuses another's, for cargo names a path package's units alike in every checkout and judges
them fresh by their sources' times, so a shared directory could serve one checkout's IR for
another's.

What is read: the IR of each production unit of this build, as cargo's own report of the build
names them, fresh or rebuilt: the libraries and binaries of the members and of the path packages
they depend on other than as a dev-dependency, and the members' integration tests, which
instantiate the libraries' generic code (a generic is compiled where it is instantiated). From each,
every `define` and its direct `call` and `invoke` edges, LLVM's intrinsics left out; then the
strongly connected components (Tarjan); then the names of the functions in a cycle, demangled here
by the v0 grammar, so every platform prints the same names.

Refused: a cycle (several functions calling round, or one calling itself) through a function of
those packages, which is one whose code a package's source holds (its own items, its impls, and the
methods its traits provide) or the drop glue of one of its types; and a cycle of drops and channel
teardown whose functions name one of their types (a type holding a channel of itself, whose drop
runs through std's `Sender` and `Receiver`). Listed with --list, not refused: a cycle inside std's
or a registry crate's code, and one among a test crate's own functions.

Not seen: a call through a trait object or a function pointer (an async fn boxing a future of
itself, a drop through `Box<dyn ...>`).
"""
import json
import os
import re
import subprocess
import sys
import tempfile
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
FIXTURES = Path(__file__).resolve().parent / "recursion-fixtures"

# ---------------------------------------------------------------------------------------------
# The v0 symbol grammar (the rustc book, "v0 Symbol Format"), printed as rust-lang's own
# demangler prints it in its alternate form, `{:#}`: no crate disambiguators and no integer
# suffixes (rustc-demangle 0.1.28, src/v0.rs). Where that printer calls itself, this one keeps a
# stack of the steps still to take, so a deep symbol costs memory, not a call stack.

BASIC = {
    "a": "i8", "b": "bool", "c": "char", "d": "f64", "e": "str", "f": "f32", "h": "u8",
    "i": "isize", "j": "usize", "l": "i32", "m": "u32", "n": "i128", "o": "u128", "p": "_",
    "s": "i16", "t": "u16", "u": "()", "v": "...", "x": "i64", "y": "u64", "z": "!",
}

# The longest name printed: rustc-demangle's own bound (src/lib.rs, MAX_SIZE, 0.1.28), which a
# symbol's backrefs could otherwise multiply past (a backref prints its target again).
MAX_NAME = 1_000_000


class Undemangled(Exception):
    """A symbol that opens as a v0 one (`_R`) and breaks the grammar."""


def escaped(text, quote):
    """`text` in `quote`s, escaped as Rust's `char::escape_debug` escapes it, with a quote of the
    other kind left as it is."""
    out = [quote]
    for c in text:
        if (quote, c) in (("'", '"'), ('"', "'")):
            out.append(c)
        elif c in "\\'\"":
            out.append("\\" + c)
        elif c in "\t\r\n\0":
            out.append({"\t": "\\t", "\r": "\\r", "\n": "\\n", "\0": "\\0"}[c])
        elif c.isprintable():
            out.append(c)
        else:
            out.append("\\u{%x}" % ord(c))
    out.append(quote)
    return "".join(out)


class V0:
    """One symbol's demangling: its printed name and its home, the crate whose source holds the
    function's code (an impl's crate, a provided method's trait's crate, an item's own crate)."""

    def __init__(self, inner):
        self.s = inner
        self.pos = 0
        self.out = []
        self.size = 0
        self.skipping = 0
        self.bound = 0
        self.steps = []
        # The home is the crate root of the path that holds the code: "armed" while the next
        # crate root met is it, "held" once found, "parked" while a part that is not it parses.
        self.home = None
        self.homing = "armed"

    def run(self, step):
        self.steps.append(step)
        while self.steps:
            step = self.steps.pop()
            step[0](*step[1:])

    def push(self, *steps):
        """Steps that run in the order given, before any step pushed earlier."""
        self.steps.extend(reversed(steps))

    # -- parsing ------------------------------------------------------------------------------

    def peek(self):
        return self.s[self.pos] if self.pos < len(self.s) else ""

    def eat(self, c):
        if self.peek() == c:
            self.pos += 1
            return True
        return False

    def next(self):
        c = self.peek()
        if not c:
            raise Undemangled("the symbol ends early")
        self.pos += 1
        return c

    def integer_62(self):
        if self.eat("_"):
            return 0
        x = 0
        while not self.eat("_"):
            c = self.next()
            if "0" <= c <= "9":
                d = ord(c) - ord("0")
            elif "a" <= c <= "z":
                d = 10 + ord(c) - ord("a")
            elif "A" <= c <= "Z":
                d = 36 + ord(c) - ord("A")
            else:
                raise Undemangled("a base-62 number holds %r" % c)
            x = x * 62 + d
        return x + 1

    def opt_integer_62(self, tag):
        return self.integer_62() + 1 if self.eat(tag) else 0

    def disambiguator(self):
        return self.opt_integer_62("s")

    def hex_nibbles(self):
        start = self.pos
        while True:
            c = self.next()
            if c == "_":
                return self.s[start:self.pos - 1]
            if c not in "0123456789abcdef":
                raise Undemangled("a constant holds %r" % c)

    def ident(self):
        """An undisambiguated identifier: its ASCII part and its punycode, if any."""
        punycode = self.eat("u")
        digits = self.next()
        if not "0" <= digits <= "9":
            raise Undemangled("an identifier's length is missing")
        if digits != "0":
            while "0" <= self.peek() <= "9":
                digits += self.next()
        self.eat("_")
        end = self.pos + int(digits)
        if end > len(self.s):
            raise Undemangled("an identifier runs past the symbol")
        text, self.pos = self.s[self.pos:end], end
        if not punycode:
            return text, ""
        cut = text.rfind("_")
        ascii, code = (text[:cut], text[cut + 1:]) if cut >= 0 else ("", text)
        if not code:
            raise Undemangled("an empty punycode identifier")
        return ascii, code

    @staticmethod
    def ident_text(ascii, code):
        if not code:
            return ascii
        standard = ascii + "-" + code if ascii else code
        try:
            return standard.encode("ascii").decode("punycode")
        except UnicodeError:
            return "punycode{" + standard + "}"

    # -- printing -----------------------------------------------------------------------------

    def emit(self, text):
        if self.skipping:
            return
        self.size += len(text)
        if self.size > MAX_NAME:
            raise Undemangled("the name runs past %d bytes" % MAX_NAME)
        self.out.append(text)

    def lit(self, text):
        self.emit(text)

    def skip(self, delta):
        self.skipping += delta

    def restore(self, pos):
        self.pos = pos

    def unbind(self, count):
        self.bound -= count

    def homing_to(self, state):
        if self.homing != "held":
            self.homing = state

    def backref(self, step):
        start = self.pos - 1
        target = self.integer_62()
        if target >= start:
            raise Undemangled("a backref points forward")
        # A skipped part's backref is not followed (its target prints nothing), unless the home
        # is still to be found in it.
        if self.skipping and self.homing != "armed":
            return
        self.push((self.restore, self.pos))
        self.pos = target
        self.push(step)

    def lifetime(self, lt):
        if self.skipping:
            return
        if lt == 0:
            self.emit("'_")
            return
        depth = self.bound - lt
        if depth < 0:
            raise Undemangled("a lifetime names no binder")
        self.emit("'" + chr(ord("a") + depth) if depth < 26 else "'_%d" % depth)

    def binder(self, body):
        count = self.opt_integer_62("G")
        if self.skipping:
            self.push(body)
            return
        if count:
            self.emit("for<")
            for i in range(count):
                if i:
                    self.emit(", ")
                self.bound += 1
                self.lifetime(1)
            self.emit("> ")
        self.push(body, (self.unbind, count))

    def sep_list(self, item, sep, count):
        if self.eat("E"):
            return
        if count[0]:
            self.emit(sep)
        count[0] += 1
        self.push(item, (self.sep_list, item, sep, count))

    def tuple_end(self, count):
        self.emit(",)" if count[0] == 1 else ")")

    def path(self, in_value):
        tag = self.next()
        if tag == "C":
            self.disambiguator()
            name = self.ident_text(*self.ident())
            if self.homing == "armed":
                self.home, self.homing = name, "held"
            self.emit(name)
        elif tag == "N":
            ns = self.next()
            if not ("A" <= ns <= "Z" or "a" <= ns <= "z"):
                raise Undemangled("a namespace is %r" % ns)
            self.push((self.path, in_value), (self.nested, ns))
        elif tag in "MX":
            self.disambiguator()
            # The impl's own path is not printed; its crate is the home.
            rest = [(self.skip, 1), (self.path, False), (self.skip, -1),
                    (self.homing_to, "parked"), (self.lit, "<"), (self.type_,)]
            if tag == "X":
                rest += [(self.lit, " as "), (self.path, False)]
            self.push(*rest, (self.lit, ">"))
        elif tag == "Y":
            # A trait's provided method: the trait's crate is the home, not the type's.
            self.homing_to("parked")
            self.push((self.lit, "<"), (self.type_,), (self.lit, " as "),
                      (self.homing_to, "armed"), (self.path, False), (self.homing_to, "parked"),
                      (self.lit, ">"))
        elif tag == "I":
            rest = [(self.path, in_value)]
            if in_value:
                rest.append((self.lit, "::"))
            # The generic arguments are never the home.
            rest += [(self.homing_to, "parked"), (self.lit, "<"),
                     (self.sep_list, (self.generic_arg,), ", ", [0]), (self.lit, ">")]
            self.push(*rest)
        elif tag == "B":
            self.backref((self.path, in_value))
        else:
            raise Undemangled("a path opens with %r" % tag)

    def nested(self, ns):
        dis = self.disambiguator()
        ascii, code = self.ident()
        named = bool(ascii or code)
        if "A" <= ns <= "Z":
            kind = {"C": "closure", "S": "shim"}.get(ns, ns)
            name = ":" + self.ident_text(ascii, code) if named else ""
            self.emit("::{%s%s#%d}" % (kind, name, dis))
        elif named:
            self.emit("::" + self.ident_text(ascii, code))

    def generic_arg(self):
        if self.eat("L"):
            self.lifetime(self.integer_62())
        elif self.eat("K"):
            self.push((self.const, False))
        else:
            self.push((self.type_,))

    def type_(self):
        if self.eat("w"):
            self.emit("#[splat] ")
        tag = self.next()
        if tag in BASIC:
            self.emit(BASIC[tag])
        elif tag in "RQ":
            self.emit("&")
            if self.eat("L"):
                lt = self.integer_62()
                if lt:
                    self.lifetime(lt)
                    self.emit(" ")
            if tag == "Q":
                self.emit("mut ")
            self.push((self.type_,))
        elif tag in "PO":
            self.emit("*const " if tag == "P" else "*mut ")
            self.push((self.type_,))
        elif tag in "AS":
            self.emit("[")
            rest = [(self.type_,)]
            if tag == "A":
                rest += [(self.lit, "; "), (self.const, True)]
            self.push(*rest, (self.lit, "]"))
        elif tag == "T":
            self.emit("(")
            count = [0]
            self.push((self.sep_list, (self.type_,), ", ", count), (self.tuple_end, count))
        elif tag == "F":
            self.binder((self.fn_sig,))
        elif tag == "D":
            self.emit("dyn ")
            self.push((self.dyn_lifetime,))
            self.binder((self.sep_list, (self.dyn_trait,), " + ", [0]))
        elif tag == "B":
            self.backref((self.type_,))
        elif tag == "W":
            self.push((self.type_,), (self.lit, " is "), (self.pat,))
        else:
            self.pos -= 1
            self.push((self.path, False))

    def fn_sig(self):
        unsafe = self.eat("U")
        abi = None
        if self.eat("K"):
            if self.eat("C"):
                abi = "C"
            else:
                ascii, code = self.ident()
                if not ascii or code:
                    raise Undemangled("an ABI is not ASCII")
                # The ABI's `-`s were written as `_`s.
                abi = ascii.replace("_", "-")
        if unsafe:
            self.emit("unsafe ")
        if abi is not None:
            self.emit('extern "%s" ' % abi)
        self.emit("fn(")
        self.push((self.sep_list, (self.type_,), ", ", [0]), (self.lit, ")"), (self.fn_return,))

    def fn_return(self):
        if not self.eat("u"):
            self.emit(" -> ")
            self.push((self.type_,))

    def dyn_trait(self):
        opened = [False]
        self.push((self.dyn_binding, opened))
        self.path_opening(opened)

    def path_opening(self, opened):
        """A trait's path with its generic arguments' `<` left open for its bindings."""
        if self.eat("B"):
            start = self.pos - 1
            target = self.integer_62()
            if target >= start:
                raise Undemangled("a backref points forward")
            if self.skipping:
                return
            self.push((self.restore, self.pos))
            self.pos = target
            self.push((self.path_opening, opened))
        elif self.eat("I"):
            opened[0] = True
            self.push((self.path, False), (self.lit, "<"),
                      (self.sep_list, (self.generic_arg,), ", ", [0]))
        else:
            opened[0] = False
            self.push((self.path, False))

    def dyn_binding(self, opened):
        if self.eat("p"):
            self.emit(", " if opened[0] else "<")
            opened[0] = True
            self.emit(self.ident_text(*self.ident()) + " = ")
            value = (self.const, False) if self.eat("K") else (self.type_,)
            self.push(value, (self.dyn_binding, opened))
        elif opened[0]:
            self.emit(">")

    def dyn_lifetime(self):
        if not self.eat("L"):
            raise Undemangled("a trait object's lifetime is missing")
        lt = self.integer_62()
        if lt:
            self.emit(" + ")
            self.lifetime(lt)

    def pat(self):
        tag = self.next()
        if tag == "R":
            self.push((self.const, False), (self.lit, "..="), (self.const, False))
        elif tag == "O":
            self.push((self.pat,), (self.pat_or,))
        elif tag == "N":
            self.emit("!null")
        else:
            raise Undemangled("a pattern opens with %r" % tag)

    def pat_or(self):
        if self.eat("E"):
            return
        if not self.peek():
            raise Undemangled("the symbol ends early")
        self.emit(" | ")
        self.push((self.pat,), (self.pat_or,))

    @staticmethod
    def uint(nibbles):
        value = nibbles.lstrip("0")
        return int(value, 16) if len(value) <= 16 and value else (0 if not value else None)

    def const(self, in_value):
        tag = self.next()
        # Only a literal stands in a generic argument without braces.
        braced = not in_value and (tag in "eAQTV" or (tag == "R" and self.peek() != "e"))
        if braced:
            self.emit("{")
            self.push((self.lit, "}"))
        if tag == "p":
            self.emit("_")
        elif tag in "htmyojaslxni":
            if tag in "aslxni" and self.eat("n"):
                self.emit("-")
            nibbles = self.hex_nibbles()
            value = self.uint(nibbles)
            self.emit(str(value) if value is not None else "0x" + nibbles)
        elif tag == "b":
            value = self.uint(self.hex_nibbles())
            if value not in (0, 1):
                raise Undemangled("a bool constant is %r" % value)
            self.emit("true" if value else "false")
        elif tag == "c":
            value = self.uint(self.hex_nibbles())
            if value is None or value > 0x10FFFF or 0xD800 <= value <= 0xDFFF:
                raise Undemangled("a char constant is no char")
            self.emit(escaped(chr(value), "'"))
        elif tag == "e":
            self.emit("*")
            self.emit(self.str_literal())
        elif tag in "RQ":
            if tag == "R" and self.eat("e"):
                self.emit(self.str_literal())
            else:
                self.emit("&" if tag == "R" else "&mut ")
                self.push((self.const, True))
        elif tag == "A":
            self.emit("[")
            self.push((self.sep_list, (self.const, True), ", ", [0]), (self.lit, "]"))
        elif tag == "T":
            self.emit("(")
            count = [0]
            self.push((self.sep_list, (self.const, True), ", ", count), (self.tuple_end, count))
        elif tag == "V":
            self.push((self.path, True), (self.variant_fields,))
        elif tag == "B":
            self.backref((self.const, in_value))
        else:
            raise Undemangled("a constant opens with %r" % tag)

    def str_literal(self):
        nibbles = self.hex_nibbles()
        if len(nibbles) % 2:
            raise Undemangled("a string constant has an odd count of nibbles")
        try:
            return escaped(bytes.fromhex(nibbles).decode("utf-8"), '"')
        except UnicodeDecodeError:
            raise Undemangled("a string constant is not UTF-8") from None

    def variant_fields(self):
        tag = self.next()
        if tag == "U":
            return
        if tag == "T":
            self.emit("(")
            self.push((self.sep_list, (self.const, True), ", ", [0]), (self.lit, ")"))
        elif tag == "S":
            self.emit(" { ")
            self.push((self.sep_list, (self.field,), ", ", [0]), (self.lit, " }"))
        else:
            raise Undemangled("a variant's fields open with %r" % tag)

    def field(self):
        self.disambiguator()
        self.emit(self.ident_text(*self.ident()) + ": ")
        self.push((self.const, True))


def is_symbol_like(text):
    return all(c.isascii() and (c.isalnum() or (c.isprintable() and not c.isspace()))
               for c in text)


def demangle(symbol):
    """The symbol's name and home crate; a symbol that is not v0 (a C function's, an LLVM
    builtin's) is its own name, with no home."""
    if not symbol.startswith("_R"):
        return symbol, None
    cut = symbol.find(".llvm.")
    if cut >= 0 and all(c in "0123456789ABCDEF@" for c in symbol[cut + 6:]):
        symbol = symbol[:cut]
    inner = symbol[2:]
    if not inner or not "A" <= inner[0] <= "Z":
        raise Undemangled("a path opens with %r" % inner[:1])
    if not inner.isascii():
        raise Undemangled("the symbol is not ASCII")
    d = V0(inner)
    d.run((d.path, True))
    name, home = "".join(d.out), d.home
    # The instantiating crate is parsed, not printed.
    if "A" <= d.peek() <= "Z":
        d.skipping, d.homing = 1, "held"
        d.run((d.path, False))
    suffix = inner[d.pos:]
    if suffix and not (suffix[0] == "." and is_symbol_like(suffix)):
        raise Undemangled("the symbol ends in %r" % suffix[:40])
    return name + suffix, home


# ---------------------------------------------------------------------------------------------
# The call graph.

DEFINE = re.compile(r'^define\b[^@]*@("(?:[^"\\]|\\.)*"|[-a-zA-Z$._0-9]+)\(')
CALL = re.compile(r'\b(?:call|invoke)\b[^@]*?@("(?:[^"\\]|\\.)*"|[-a-zA-Z$._0-9]+)\(')


def read_graph(paths):
    graph = {}
    for path in paths:
        current = None
        with open(path, "r", encoding="utf-8", errors="replace") as lines:
            for line in lines:
                if current is None:
                    m = DEFINE.match(line)
                    if m:
                        current = m.group(1).strip('"')
                        graph.setdefault(current, set())
                    continue
                if line.startswith("}"):
                    current = None
                    continue
                for c in CALL.finditer(line):
                    callee = c.group(1).strip('"')
                    if not callee.startswith("llvm."):
                        graph[current].add(callee)
    return graph


def cycles(graph):
    """Every strongly connected component that is a cycle: several functions, or one calling
    itself (Tarjan's algorithm, its depth-first walk kept on a list)."""
    index, low, on_stack, stack, found, counter = {}, {}, set(), [], [], 0
    for start in graph:
        if start in index:
            continue
        index[start] = low[start] = counter
        counter += 1
        stack.append(start)
        on_stack.add(start)
        work = [(start, sorted(graph[start]))]
        while work:
            node, pending = work[-1]
            if pending:
                callee = pending.pop()
                if callee not in index:
                    index[callee] = low[callee] = counter
                    counter += 1
                    stack.append(callee)
                    on_stack.add(callee)
                    work.append((callee, sorted(graph.get(callee, ()))))
                elif callee in on_stack:
                    low[node] = min(low[node], index[callee])
                continue
            work.pop()
            if work:
                parent = work[-1][0]
                low[parent] = min(low[parent], low[node])
            if low[node] == index[node]:
                component = []
                while True:
                    member = stack.pop()
                    on_stack.discard(member)
                    component.append(member)
                    if member == node:
                        break
                if len(component) > 1 or node in graph.get(node, ()):
                    found.append(component)
    return found


# ---------------------------------------------------------------------------------------------
# What is refused.

# The drop glue of a type, which rustc 1.98 names `drop_glue` and earlier ones `drop_in_place`.
GLUE = ("core::ptr::drop_glue::<", "core::ptr::drop_in_place::<")
# The functions of a drop and of std's channel teardown.
TEARDOWN = ("drop_glue", "drop_in_place", " as core::ops::drop::Drop>::drop", "::release",
            "::disconnect", "::discard_all_messages", "Channel")
# std's algorithms, whose recursion is bounded by their input and is std's own.
ALGORITHMS = ("core::slice::sort", "<alloc::collections::btree")


def leading_crate(name):
    return name.split("::", 1)[0].lstrip("<&*[(")


class Judge:
    def __init__(self, ours):
        self.ours = ours
        self.naming = re.compile(r"\b(" + "|".join(sorted(map(re.escape, ours))) + r")::")

    def ours_fn(self, name, home):
        if home in self.ours:
            return True
        return any(name.startswith(g) and leading_crate(name[len(g):]) in self.ours for g in GLUE)

    def refused(self, members):
        if any(self.ours_fn(name, home) for name, home in members):
            return True
        names = [name for name, _ in members]
        teardown = all(any(t in name for t in TEARDOWN) for name in names)
        algorithm = all(name.startswith(ALGORITHMS) for name in names)
        return teardown and not algorithm and any(self.naming.search(n) for n in names)


def shape(names):
    """A cycle's functions with their generic arguments cut (`Name<..>`, `path::<..>`), so that one
    cycle instantiated for several types is one shape; an impl's `<T as Trait>`, which opens a
    segment, stays, its own generic arguments cut."""
    def cut(name):
        out, groups = [], []
        for i, c in enumerate(name):
            before = name[i - 1] if i else ""
            inside = bool(groups) and groups[-1]
            if c == "<":
                generic = inside or before.isalnum() or before in ("_", ":")
                if generic and not inside and out[-2:] == [":", ":"]:
                    del out[-2:]
                groups.append(generic)
                if not generic:
                    out.append(c)
            elif c == ">" and before != "-" and groups:
                if not groups.pop():
                    out.append(c)
            elif not inside:
                out.append(c)
        return "".join(out)
    return tuple(sorted({cut(n) for n in names}))


def judge(graph, ours):
    """The cycles, demangled and split into the refused and the listed."""
    found = cycles(graph)
    names, failed = {}, []
    for symbol in sorted({s for c in found for s in c}):
        try:
            names[symbol] = demangle(symbol)
        except Undemangled as e:
            failed.append((symbol, str(e)))
    if failed:
        for symbol, why in failed:
            print("UNDEMANGLED:", symbol, "-", why, file=sys.stderr)
        sys.exit("the demangler does not read every symbol in a cycle: extend it (v0 grammar)")
    j = Judge(ours)
    refusals, listed = [], []
    for component in found:
        members = sorted({names[s] for s in component})
        (refusals if j.refused(members) else listed).append([n for n, _ in members])
    return refusals, listed


def report(refusals, listed, show_listed):
    distinct = {}
    for names in refusals:
        distinct.setdefault(shape(names), names)
    print("%d cycles: %d refused (%d distinct), %d listed (std, registry crates, test crates)"
          % (len(refusals) + len(listed), len(refusals), len(distinct), len(listed)))
    for names in sorted(distinct.values(), key=len, reverse=True):
        print("- %d fn:" % len(names))
        for n in names:
            print("    " + n)
    if show_listed:
        for s in sorted({shape(names) for names in listed}):
            print("  listed: " + " | ".join(s))


# ---------------------------------------------------------------------------------------------
# The build.

def production(meta):
    """The path packages whose code runs in production: the members and every path package they
    reach by normal or build dependencies. One reached only as a dev-dependency is test support."""
    local = {p["id"] for p in meta["packages"] if p["source"] is None}
    edges = {node["id"]: [d["pkg"] for d in node["deps"]
                          if any(k["kind"] in (None, "build") for k in d["dep_kinds"])]
             for node in meta["resolve"]["nodes"]}
    found, frontier = set(meta["workspace_members"]), list(meta["workspace_members"])
    while frontier:
        for dep in edges.get(frontier.pop(), ()):
            if dep in local and dep not in found:
                found.add(dep)
                frontier.append(dep)
    return found


def same_file(a, b):
    if a.stat().st_size != b.stat().st_size:
        return False
    with open(a, "rb") as x, open(b, "rb") as y:
        while True:
            chunk = x.read(1 << 20)
            if chunk != y.read(1 << 20):
                return False
            if not chunk:
                return True


def unit_ir(artifact, deps):
    """The IR file of one unit cargo reported: a library's sits beside its `.rmeta`, a test's
    beside its executable, and a binary's beside the copy in deps/ of the executable cargo
    reports (the copy that is byte for byte the same; deps/ also keeps earlier builds' copies).
    A binary's copy is named with the unit's hash, or on an MSVC target by its crate alone: cargo
    leaves the hash out of a path package's executables there, so their debug files keep one name
    (cargo's `should_use_metadata`)."""
    kinds = set(artifact["target"]["kind"])
    crate = artifact["target"]["name"].replace("-", "_")
    if kinds & {"lib", "rlib"}:
        for f in artifact["filenames"]:
            m = re.fullmatch(r"lib(.+-[0-9a-f]+)\.(rmeta|rlib)", Path(f).name)
            if m and Path(f).parent == deps:
                return deps / (m.group(1) + ".ll")
        sys.exit("no library in deps/ for %s: %s" % (crate, artifact["filenames"]))
    executable = Path(artifact["executable"])
    if "test" in kinds:
        return executable.with_suffix(".ll") if executable.suffix == ".exe" else \
            executable.parent / (executable.name + ".ll")
    for candidate in sorted(deps.glob(crate + "*")):
        if candidate.suffix not in ("", ".exe") or candidate.name.count(".") > 1:
            continue
        if candidate.stem != crate and not candidate.stem.startswith(crate + "-"):
            continue
        if same_file(candidate, executable):
            return candidate.with_suffix(".ll") if candidate.suffix == ".exe" else \
                candidate.parent / (candidate.name + ".ll")
    sys.exit("no copy in deps/ of the binary %s" % executable)


def build(target_dir):
    meta = json.loads(subprocess.run(
        ["cargo", "metadata", "--format-version", "1", "--locked"],
        cwd=ROOT, check=True, capture_output=True, text=True).stdout)
    runs = production(meta)
    target_dir.mkdir(parents=True, exist_ok=True)
    marker = target_dir / "recursion-tree"
    if marker.exists() and marker.read_text().strip() != str(ROOT):
        sys.exit("%s holds the IR of %s: give this tree a directory of its own"
                 % (target_dir, marker.read_text().strip()))
    marker.write_text(str(ROOT) + "\n")
    env = dict(os.environ, CARGO_TARGET_DIR=str(target_dir))
    for profile in ("DEV", "TEST"):
        env.update({"CARGO_PROFILE_%s_OPT_LEVEL" % profile: "0",
                    "CARGO_PROFILE_%s_CODEGEN_UNITS" % profile: "1",
                    "CARGO_PROFILE_%s_INCREMENTAL" % profile: "false",
                    "CARGO_PROFILE_%s_DEBUG" % profile: "0"})
    # cargo reads CARGO_ENCODED_RUSTFLAGS before RUSTFLAGS; the flag joins whichever is set.
    if "CARGO_ENCODED_RUSTFLAGS" in env:
        flags = env["CARGO_ENCODED_RUSTFLAGS"]
        env["CARGO_ENCODED_RUSTFLAGS"] = (flags + "\x1f" if flags else "") + "--emit=llvm-ir"
    else:
        env["RUSTFLAGS"] = (env.get("RUSTFLAGS", "") + " --emit=llvm-ir").strip()
    cargo = subprocess.Popen(
        ["cargo", "build", "--workspace", "--lib", "--bins", "--test", "*", "--all-features",
         "--locked", "--message-format=json-render-diagnostics"],
        cwd=ROOT, env=env, stdout=subprocess.PIPE, text=True)
    artifacts = [m for m in map(json.loads, cargo.stdout) if m.get("reason") == "compiler-artifact"]
    if cargo.wait() != 0:
        sys.exit("the build failed")
    deps = target_dir / "debug" / "deps"
    ours, files = set(), set()
    for a in artifacts:
        kinds = set(a["target"]["kind"])
        if a["package_id"] not in runs or kinds & {"custom-build", "proc-macro"}:
            continue
        if kinds & {"lib", "rlib", "bin"}:
            ours.add(a["target"]["name"].replace("-", "_"))
        if kinds & {"lib", "rlib", "bin", "test"}:
            files.add(unit_ir(a, deps))
    missing = sorted(str(f) for f in files if not f.exists())
    if missing:
        sys.exit("no IR for %d units (was this directory built without --emit=llvm-ir?): %s"
                 % (len(missing), missing[:4]))
    return ours, sorted(files)


# ---------------------------------------------------------------------------------------------
# The self-test: rust-lang's demangler's own cases, and fixture crates holding each kind of cycle.

DEMANGLED = [
    ("_RNvC6_123foo3bar", "123foo::bar"),
    ("_RC4f128", "f128"),
    ("_RNqCs4fqI2P2rA04_11utf8_identsu30____7hkackfecea1cbdathfdh9hlq6y",
     "utf8_idents::საჭმელად_გემრიელი_სადილი"),
    ("_RNCNCNgCs6DXkGYLi8lr_2cc5spawn00B5_", "cc::spawn::{closure#0}::{closure#0}"),
    ("_RNCINkXs25_NgCsbmNqQUJIY6D_4core5sliceINyB9_4IterhENuNgNoBb_4iter8iterator8Iterator"
     "9rpositionNCNgNpB9_6memchr7memrchrs_0E0Bb_",
     "<core::slice::Iter<u8> as core::iter::iterator::Iterator>::rposition::"
     "<core::slice::memchr::memrchr::{closure#1}>::{closure#0}"),
    ("_RINbNbCskIICzLVDPPb_5alloc5alloc8box_freeDINbNiB4_5boxed5FnBoxuEp6OutputuEL_ECs1iopQbuB"
     "iw2_3std", "alloc::alloc::box_free::<dyn alloc::boxed::FnBox<(), Output = ()>>"),
    ("_RMC0DNtC5krate5Traitp1NKj0_EL_", "<dyn krate::Trait<N = 0>>"),
    ("_RMC0WmRm1_m9_", "<u32 is 1..=9>"),
    ("_RMC0WmORm1_m2_Rm5_m6_E", "<u32 is 1..=2 | 5..=6>"),
    ("_RNvMNtCs5CcWRJzwAYz_4core6optionINtB2_6OptionFwTlEEuE6unwrapCsiksvdpJ6Jnj_14splat_mangling",
     "<core::option::Option<fn(#[splat] (i32,))>>::unwrap"),
    ("_RMs4_NvCsiksvdpJ6Jnj_14splat_mangling4mainINtB3_4TypeFwTmaEdEuE",
     "<splat_mangling::main::Type<fn(#[splat] (u32, i8), f64)>>"),
    ("_RMC0INtC8arrayvec8ArrayVechKj7b_E", "<arrayvec::ArrayVec<u8, 123>>"),
    ("_RIC0Kp" "E", "::<_>"),
    ("_RIC0Koff00ff00ff00ff00ff_E", "::<0xff00ff00ff00ff00ff>"),
    ("_RIC0Kanb_E", "::<-11>"),
    ("_RIC0Kb1_E", "::<true>"),
    ("_RIC0Kc22_E", "::<'\"'>"),
    ("_RIC0Kca_E", "::<'\\n'>"),
    ("_RIC0Kc2202_E", "::<'∂'>"),
    ("_RIC0Ke27_E", "::<{*\"'\"}>"),
    ("_RIC0Ke090a_E", "::<{*\"\\t\\n\"}>"),
    ("_RIC0KRee28882c3bc_E", "::<\"∂ü\">"),
    ("_RIC0KRp" "E", "::<{&_}>"),
    ("_RIC0KRRRe_E", "::<{&&\"\"}>"),
    ("_RIC0KQAEE", "::<{&mut []}>"),
    ("_RIC0KAAh1_h2_EAh3_h4_EEE", "::<{[[1, 2], [3, 4]]}>"),
    ("_RIC0KTj0_EE", "::<{(0,)}>"),
    ("_RIC0KTRe616263_c78_RAh1_h2_h3_EEE", "::<{(\"abc\", 'x', &[1, 2, 3])}>"),
    ("_RIC0KVNvINtNtC4core6option6OptionjE4SomeTj0_EE",
     "::<{core::option::Option::<usize>::Some(0)}>"),
    ("_RIC0KVNtC3foo3BarS1sRe616263_2chc78_5sliceRAh1_h2_h3_EEE",
     "::<{foo::Bar { s: \"abc\", ch: 'x', slice: &[1, 2, 3] }}>"),
    ("_RMC0TTTTTTpB8_EB7_EB6_EB5_EB4_EB3_E",
     "<((((((_, _), (_, _)), ((_, _), (_, _))), (((_, _), (_, _)), ((_, _), (_, _)))), "
     "((((_, _), (_, _)), ((_, _), (_, _))), (((_, _), (_, _)), ((_, _), (_, _))))), "
     "(((((_, _), (_, _)), ((_, _), (_, _))), (((_, _), (_, _)), ((_, _), (_, _)))), "
     "((((_, _), (_, _)), ((_, _), (_, _))), (((_, _), (_, _)), ((_, _), (_, _))))))>"),
    ("_RC3foo.llvm.9D1C9369", "foo"),
    ("_RNvC9backtrace3foo.llvm.A5310EB9", "backtrace::foo"),
    ("_RNvNtNtNtNtCs92dm3009vxr_4rand4rngs7adapter9reseeding4fork23FORK_HANDLER_REGISTERED.0.0",
     "rand::rngs::adapter::reseeding::fork::FORK_HANDLER_REGISTERED.0.0"),
]


def self_test():
    for symbol, expected in DEMANGLED:
        got, _ = demangle(symbol)
        if got != expected:
            sys.exit("demangled %s\n  as %s\n  not %s" % (symbol, got, expected))
    # A deep symbol is read without a deep stack: ten thousand nested references.
    deep = "_RMC0" + "R" * 10_000 + "p"
    if demangle(deep)[0] != "<" + "&" * 10_000 + "_>":
        sys.exit("a deep symbol misread")
    with tempfile.TemporaryDirectory() as tmp:
        out = Path(tmp)
        common = ["rustc", "--edition", "2024", "--crate-type", "lib", "-C", "opt-level=0",
                  "-C", "codegen-units=1", "-C", "debuginfo=0", "--emit=llvm-ir,metadata,link",
                  "--out-dir", str(out)]
        subprocess.run(common + ["--crate-name", "theirs_fixture",
                                 str(FIXTURES / "theirs.rs")], cwd=ROOT, check=True)
        subprocess.run(common + ["--crate-name", "hyper_fixture", "--extern",
                                 "theirs_fixture=%s" % (out / "libtheirs_fixture.rlib"),
                                 str(FIXTURES / "ours.rs")], cwd=ROOT, check=True)
        refusals, listed = judge(read_graph(sorted(out.glob("*.ll"))), {"hyper_fixture"})
    expected = {
        "a function calling itself": lambda n: "hyper_fixture::walk" in n,
        "two functions calling round": lambda n: {"hyper_fixture::even", "hyper_fixture::odd"} <= set(n),
        "a type holding itself, dropped": lambda n: "core::ptr::drop_glue::<hyper_fixture::Node>" in n,
        "a type holding a channel of itself, dropped": lambda n: "core::ptr::drop_glue::<hyper_fixture::Reply>" in n,
        "an impl of the crate's trait on a foreign type": lambda n: "<u64 as hyper_fixture::Visit>::visit" in n,
    }
    unexpected = [n for n in refusals if not any(test(n) for test in expected.values())]
    absent = [what for what, test in expected.items() if not any(test(n) for n in refusals)]
    named = lambda prefix: any(any(m.startswith(prefix) for m in n) for n in listed)
    listed_absent = [p for p in ("theirs_fixture::walk", "theirs_fixture::visit::<",
                                 "core::slice::sort::") if not named(p)]
    loop = [n for n in refusals + listed if any("iterate" in m or "step" in m for m in n)]
    if unexpected or absent or listed_absent or loop:
        report(refusals, listed, True)
        sys.exit("self-test: refused but not expected %s; expected but not refused %s; not "
                 "listed %s; a loop found as a cycle %s" % (unexpected, absent, listed_absent, loop))
    print("self-test: %d demanglings; %d refused cycles, each expected; %d listed"
          % (len(DEMANGLED) + 1, len(refusals), len(listed)))


def main():
    args = sys.argv[1:]
    if "--self-test" in args:
        self_test()
        return
    target_dir = ROOT / "target" / "recursion"
    if "--target-dir" in args:
        target_dir = Path(args[args.index("--target-dir") + 1]).resolve()
    started = time.monotonic()
    ours, files = build(target_dir)
    built = time.monotonic()
    graph = read_graph(files)
    refusals, listed = judge(graph, ours)
    print("built in %.0f s; read %d units, %d functions, in %.0f s"
          % (built - started, len(files), len(graph), time.monotonic() - built))
    report(refusals, listed, "--list" in args)
    sys.exit(1 if refusals else 0)


if __name__ == "__main__":
    main()
