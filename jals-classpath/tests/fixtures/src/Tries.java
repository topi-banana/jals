package demo;

// Provenance for Tries.class — `try` / `catch` / `finally` structuring. The exception table is the
// only record of what was protected: a `catch` clause is a `handler_pc` plus the `catch_type`s
// aimed at it (a multi-catch is several entries sharing one handler), and a `finally` is a
// `catch_type` of 0 whose body `javac` *duplicates* onto every exit. These methods cover both the
// shapes that read back and the ones that must fall back. Compiled with -g so the
// LocalVariableTable can name a catch parameter, and prove the slot is its own:
//     javac -parameters -g -d out jals-classpath/tests/fixtures/src/Tries.java
//     cp out/demo/Tries.class jals-classpath/tests/fixtures/
public class Tries {
    int value;
    int other;

    // The minimal shape: both the try body and the handler leave by `return`, so no edge names a
    // join at all. Recovered only because the region itself ends by exiting.
    public int catchAndReturn(String s) {
        try {
            return parse(s);
        } catch (java.io.IOException e) {
            return -1;
        }
    }

    // The handler falls into the join and the try body's trailing `goto` names it.
    public void catchAndFallThrough(String s) {
        try {
            this.value = parse(s);
        } catch (java.io.IOException e) {
            this.value = -1;
        }
    }

    // Statements follow the `try`, so the join is a real block rather than the method's tail.
    public int trailing(String s) {
        int r = 0;
        try {
            r = parse(s);
        } catch (java.io.IOException e) {
            r = -1;
        }
        return r + 1;
    }

    // Two clauses, each with its own handler. Both catch parameters are named `e` and javac gives
    // them the same slot, so each must be resolved from the LocalVariableTable entry covering its
    // own handler rather than from the slot alone.
    public int twoCatches(String s) {
        try {
            return parse(s);
        } catch (java.io.IOException e) {
            return -1;
        } catch (RuntimeException e) {
            return -2;
        }
    }

    // A multi-catch: two exception-table entries share one `handler_pc`. The clause's types come
    // from those `catch_type`s — the LocalVariableTable records the parameter as their least upper
    // bound (`RuntimeException` here), which is a type the source never wrote.
    public void multiCatch(String s) {
        try {
            this.value = Integer.parseInt(s);
        } catch (NumberFormatException | NullPointerException e) {
            this.value = -1;
        }
    }

    // An empty handler: after the entry `astore` is consumed there is nothing left to replay.
    public void emptyCatch(String s) {
        try {
            this.value = Integer.parseInt(s);
        } catch (RuntimeException e) {
        }
    }

    // A `try` inside a `try`. The outer protected range covers the inner statement *and* the inner
    // handler, so the nesting is readable from the ranges alone.
    public void nestedTry(String s) {
        try {
            try {
                this.value = Integer.parseInt(s);
            } catch (NumberFormatException e) {
                this.value = 1;
            }
        } catch (RuntimeException e) {
            this.value = 2;
        }
    }

    // A `for` inside a `try`. The counter dies at the closing brace and the catch parameter is born
    // right after it, so `javac` gives them one slot — and a slot holding two different variables is
    // the reuse M3 declines to split. Bails, and has to: the counter's declaration would otherwise
    // move into the `for` header (M9) while nothing declared it, which parses cleanly and only
    // `javac` rejects.
    public int loopInTry(int n) {
        int total = 0;
        try {
            for (int i = 0; i < n; i++) {
                total = total + i;
            }
        } catch (RuntimeException e) {
            total = -1;
        }
        return total;
    }

    // `finally` with no `catch`: the body is duplicated onto the normal exit and into the
    // catch-all handler, which rethrows. Two copies, and they must agree instruction for
    // instruction before either is folded away.
    public void tryFinally() throws java.io.IOException {
        try {
            mayThrow();
        } finally {
            this.value = 9;
        }
    }

    // `catch` *and* `finally`, with the handler falling through: three copies of the finalizer —
    // the try body's normal exit, the catch clause's normal exit, and the catch-all handler.
    public void tryCatchFinally() {
        try {
            mayThrow();
        } catch (java.io.IOException e) {
            this.value = -1;
        } finally {
            this.value = 9;
        }
    }

    // The clause leaves by `throw`, so it gets no copy of its own: two copies, not three. The
    // `any` entry covering the clause stops at the handler's own `astore`.
    public void tryCatchFinallyThrowing() {
        try {
            mayThrow();
        } catch (java.io.IOException e) {
            throw new IllegalStateException("boom");
        } finally {
            this.value = 9;
        }
    }

    // A loop whose header *is* the try's entry block. The counter is a parameter, so no fresh slot
    // competes with the catch parameter — which is what lets this shape be recovered at all, and
    // what makes it the case that proves a `try` is looked for before a loop header is.
    public void loopInTryBody(int n) {
        try {
            while (n > 0) {
                n = n - 1;
            }
        } catch (RuntimeException e) {
            this.value = -1;
        }
    }

    // A `for` under a `finally`. A finalizer has no catch parameter, so the counter keeps a slot of
    // its own and its declaration can move into the header — which is the case that needs the
    // hoisted declaration dropped, a duplicate `javac` rejects and a parser accepts.
    public int loopInTryFinally(int n) {
        int total = 0;
        try {
            for (int i = 0; i < n; i++) {
                total = total + i;
            }
        } finally {
            this.value = 9;
        }
        return total;
    }

    // An empty finalizer: the handler is nothing but its entry `astore` and the rethrow, so the
    // body between them is empty.
    public void emptyFinally() throws java.io.IOException {
        try {
            mayThrow();
        } finally {
        }
    }

    // Several statements in the finalizer: still one straight run, so the copies match.
    public void tryFinallyBlock() throws java.io.IOException {
        try {
            mayThrow();
        } finally {
            this.value = 9;
            this.other = 10;
        }
    }

    // --- the shapes that must fall back to the safe body ---

    // A branch inside the finalizer. What follows a copy differs by copy — the normal exit
    // continues past the statement, the handler rethrows — so the `if`'s arms jump to different
    // places and the copies are not identical after all. The equality check is what notices.
    public void finallyWithBranch(int n) throws java.io.IOException {
        try {
            mayThrow();
        } finally {
            if (n > 0) {
                this.value = 1;
            } else {
                this.value = 2;
            }
        }
    }

    // `return` inside a `try` that has a `finally`: javac spills the return value to a synthetic
    // slot the LocalVariableTable never names, so the local cannot be typed.
    public int returnInsideTryFinally(int n) {
        try {
            return n;
        } finally {
            this.value = 9;
        }
    }

    // The catch parameter's slot is reused by a loop variable, so no single entry owns it.
    public int sharedCatchSlot(int n) {
        int total = 0;
        for (int i = 0; i < n; i++) {
            total = total + i;
        }
        try {
            total = total + parse("1");
        } catch (java.io.IOException e) {
            total = -1;
        }
        return total;
    }

    // A `synchronized` block compiles to a catch-all handler that looks like a `finally`. It is
    // separated by the `monitorenter` / `monitorexit` pair, which the simulator does not model.
    public void synchronizedBlock(Object lock) {
        synchronized (lock) {
            this.value = 2;
        }
    }

    // `return` inside the finalizer discards a pending exception — a different meaning from
    // falling out of it, and one the rethrowing copy can no longer express.
    public int finallyWithReturn(int n) {
        try {
            mayThrow();
        } finally {
            return n;
        }
    }

    // A `try` inside the finalizer puts exception-table entries inside every copy, so folding the
    // copies away would silently drop handlers.
    public void tryInsideFinally() throws java.io.IOException {
        try {
            mayThrow();
        } finally {
            try {
                mayThrow();
            } catch (java.io.IOException e) {
                this.value = -1;
            }
        }
    }

    // Two `return`s inside one `try` split the protected range in two, and a hole that means
    // "control left here" is not the same as one that means "this is not protected".
    public int twoReturnsInTry(String s) {
        try {
            if (s.isEmpty()) {
                return 0;
            }
            return parse(s);
        } catch (java.io.IOException e) {
            return -1;
        }
    }

    // try-with-resources lowers to a synthetic close/addSuppressed sequence, which is out of scope.
    public void tryWithResources(java.io.Reader r) {
        try (java.io.Reader in = r) {
            this.value = in.read();
        } catch (java.io.IOException e) {
            this.value = -1;
        }
    }

    int parse(String s) throws java.io.IOException {
        return s.length();
    }

    void mayThrow() throws java.io.IOException {
    }
}
