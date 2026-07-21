package demo;

// Provenance for Switches.class — exercises the M8 switch structuring: the two table encodings,
// stacked labels, fall-through, every join shape javac produces (a break-derived join, a
// default-less fall-out, all-arms-return), a default written first / in the middle, nested
// conditionals inside an arm, an arm whose paths break and return, and the two desugared switches
// that must still fall back.
// Compiled with `javac` (JDK 25):
//     javac -parameters -g -d out jals-classpath/tests/fixtures/src/Switches.java
//     cp out/demo/Switches*.class jals-classpath/tests/fixtures/
public class Switches {
    private int value;

    // Dense keys with breaks and no `default:` — javac aims the default offset at the fall-out, so
    // this is the shape whose join can only be read off the arms' break edges.
    public void dense(int x) {
        switch (x) {
            case 0:
                this.value = 10;
                break;
            case 1:
                this.value = 11;
                break;
            case 2:
                this.value = 12;
                break;
        }
        this.value = this.value + 1;
    }

    // Sparse keys (a lookupswitch) with a real `default:` written last.
    public int sparse(int x) {
        int r;
        switch (x) {
            case 1:
                r = 100;
                break;
            case 100:
                r = 1;
                break;
            default:
                r = -1;
        }
        return r;
    }

    // Two keys sharing one arm, plus a gap key that only `default` covers.
    public int stacked(int x) {
        switch (x) {
            case 1:
            case 2:
                return 12;
            case 4:
                return 4;
            default:
                return 0;
        }
    }

    // Deliberate fall-through: `case 1` runs into `case 2`.
    public int fallThrough(int x) {
        int r = 0;
        switch (x) {
            case 1:
                r = r + 1;
            case 2:
                r = r + 2;
                break;
            case 3:
                r = r + 3;
        }
        return r;
    }

    // Every arm returns and there is no `default:` — nothing names a join, so it comes from the
    // default offset instead.
    public int allReturn(int x) {
        switch (x) {
            case 0:
                return 10;
            case 1:
                return 11;
        }
        return -1;
    }

    // `default:` written in the middle of the case order.
    public int defaultMiddle(int x) {
        switch (x) {
            case 1:
                return 1;
            default:
                return 0;
            case 5:
                return 5;
        }
    }

    // A plain `if` inside an arm — its skip target stays inside the arm.
    public void ifInArm(int x, boolean y) {
        switch (x) {
            case 1:
                if (y) {
                    this.value = 1;
                }
                break;
            case 2:
                this.value = 2;
                break;
        }
        this.value = this.value + 1;
    }

    // An `if`/`else` inside an arm. javac collapses the then-branch's exit goto straight to the
    // switch join, so this only structures once `break_target` is threaded through the regions.
    public void ifElseInArm(int x, boolean y) {
        switch (x) {
            case 1:
                if (y) {
                    this.value = 1;
                } else {
                    this.value = 2;
                }
                break;
            case 2:
                this.value = 3;
                break;
        }
        this.value = this.value + 1;
    }

    // An arm with two exits: an inner `if` that breaks out, and a tail that returns. The arm as a
    // whole reaches the join, but its *tail* does not — so no trailing `break;` may be emitted
    // after the `return`, which JLS 14.21 would reject as unreachable.
    public int breakThenReturnInArm(int x, boolean y) {
        int r = 0;
        switch (x) {
            case 1:
                if (y) {
                    r = 1;
                    break;
                }
                return -1;
            case 2:
                r = 2;
        }
        return r;
    }

    // A switch on `char`.
    public int vowel(char c) {
        switch (c) {
            case 'a':
            case 'e':
                return 1;
            default:
                return 0;
        }
    }

    // Desugars to a synthetic `$SwitchMap$` ordinal table — must fall back rather than render a
    // reference to a synthetic class.
    public int onEnum(Color c) {
        switch (c) {
            case RED:
                return 1;
            case GREEN:
                return 2;
            default:
                return 0;
        }
    }

    // Desugars to a two-stage hashCode()/equals() dispatch — must fall back.
    public int onString(String s) {
        switch (s) {
            case "a":
                return 1;
            case "bb":
                return 2;
            default:
                return 0;
        }
    }

    public enum Color {
        RED,
        GREEN,
        BLUE
    }
}
