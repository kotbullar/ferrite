"""The on-radio UI test framework (tools/uitest.py runs it): tests, the test context, the screen readback.

A test is a function taking the context `t`, tagged with the manual rule IDs it checks:

    from .core import test

    @test("MENU-1", "MENU-13")
    def quick_settings_rows(t):
        t.home()
        u = t.keys("MENU")              # keys, then the screen read back (a U)
        t.eq(u.id, "Quick", "MENU on home opens quick settings")
        t.check(len(u.rows) == 11, "nine numbered rows, RF gain and More", u.rows)

`where="radio"` marks a test the simulator can't model: a simulator run doesn't collect it at all (nor one whose
needs= has a CAPS entry the simulator never has: a bench build, the cable log, a flash), so radio-only tests never
count as the simulator's skips (`--list` marks them). `slow=True` one that waits on a timer (on the radio only with
--slow; the simulator always runs them, in parallel); `needs=` the build flavours or the things it needs (CAPS
below: a bench build, a Bluetooth link, a factory reset allowed, a flash around the run, ...), skipped (saying
which) when the run doesn't have them. `sim_args=("--no-ssb-patch",)`: the simulator state the test needs, given to
a simulator started for it alone (--sim auto). `pages=("Zones", "Browse")` names the screens the test visits (their readback ids, the `screen`
line's first word): `tools/uitest.py --page zones` runs the tests that visit a page, and `--check-pages` runs the
tests and reports any whose tags differ from the screens they actually read back (readbacks inside `t.home()`
don't count); `--fix-pages` writes what they visited into the tags. Safe by construction: the context has no PTT and the link's remote keys never transmit
(LINK-19) but for the crossband repeater (XB-20), which only modes.py's crossband tests start, on tx-dry, both sides
on the approved test frequencies; a setting a test changes is put back by the runner after it (from the snapshot taken at the start), and
`t.set()` only takes settings outside UNSAFE.
"""
import re
import time

REGISTRY = []

# Home's readback ids: home, and Low Power Mode's own home (PWR-24.7).
HOMES = ("Home", "LowPower")

# Settings a test never changes: they could transmit (VOX, beacon, digipeat, CW ID), take the cable away from the
# link (cable port, KISS, cable log, terminal colour) or the radio off (auto power off), or need the radio's owner.
UNSAFE = {"0 8 2 1", "0 5 6 1", "0 5 6 2", "0 5 7 1", "0 8 6 2", "0 9 2 3", "0 5 8", "0 9 9 5", "0 9 9 3",
          "0 7 8", "0 9 2 1", "0 9 3 1", "0 9 3 2"}


# What a test may need beyond a build flavour (needs=("bench",), needs=("tx-dry", "reset")): the tag, whether the
# simulator provides it, and what gives it on the radio. A build flavour in needs= is one the build must be.
BUILDS = ("receive", "tx", "tx-dry")
# "tx-dry" in needs= means a transmit build with the PA drive held at 0 now: Dry TX on (LINK-35 "tx+dry"; the
# tx-bench build starts so, TX-48.1, decision 193), or an older radio's tx-dry build.
DRY = ("tx-dry", "tx+dry")
CAPS = {
    "bench": (False, "a bench build (tx-bench or bring-up): bk, gpio, mem, si, rx (TERM-38)"),
    "keyup": (True, "Dry TX on (the tx-bench build at power-on; PA held at 0) for remote key-ups that must be refused"),
    "bt": (True, "a Bluetooth link beside the cable: --bt ble:walkie-talkie (0 9 2 1 on)"),
    "cablelog": (False, "keys over Bluetooth (--port ble:...) and the cable log on --cablelog PORT"),
    "flash": (False, "a flash around the run: --flash before, tools/flash.py, --flash after"),
    "first-install": (True, "--flash before marks a first install (a radio new to Ferrite or wiped): --with first-install"),
    "reset": (True, "a factory reset (0 9 7 2) allowed: --with reset (the test reloads the dump after)"),
    "fm-station": (True, "a local FM station with RDS and stereo pilot (the simulator has them): --with fm-station"),
    "signal": (False, "a signal to receive (a second radio or a strong local one): --with signal"),
    "fix": (True, "a GPS fix (the simulator has one): --with fix outdoors"),
    "pin": (True, "a PIN may be set by keys (it stays set, the lock off, after: Change PIN has no none): --with pin"),
}


def radio_only(tst):
    """The simulator can never run this test: where="radio", or a need it never has (CAPS). A simulator run doesn't
    collect it (not a skip)."""
    return tst.where == "radio" or any(n in CAPS and not CAPS[n][0] for n in (tst.needs or ()))


def unmet(needs, build, sim, bench, opts):
    """Why a test with these needs can't run here ("" when it can)."""
    if not needs:
        return ""
    builds = [n for n in needs if n in BUILDS]
    if builds and build not in builds and not ("tx-dry" in builds and build in DRY) and not ("keyup" in needs and sim):
        return f"needs a {' or '.join(builds)} build (this is {build})"
    given = {x.strip() for x in (getattr(opts, "with_", "") or "").split(",") if x.strip()}
    for n in needs:
        if n in BUILDS:
            continue
        sim_has, what = CAPS[n]
        if n == "bench":
            ok = bench and not sim
        elif n == "keyup":
            ok = sim or build in DRY
        elif n == "bt":
            ok = bool(getattr(opts, "bt", None))
        elif n == "cablelog":
            ok = bool(getattr(opts, "cablelog", None)) and not sim
        elif n == "flash":
            ok = bool(getattr(opts, "flash", None)) and not sim
        else:
            ok = (sim and sim_has) or n in given
        if not ok:
            return f"needs {what}"
    return ""


# How long `T.stable` reads back for a settled screen before it takes the last readback as it is.
STABLE_S = 1.0


class Fail(Exception):
    pass


class Skip(Exception):
    pass


class Test:
    def __init__(self, fn, rules, where, slow, needs, smoke, xfail, pages, sim_args=()):
        self.fn, self.rules, self.where, self.slow, self.needs, self.smoke = fn, list(rules), where, slow, needs, smoke
        self.sim_args = tuple(sim_args or ())
        self.xfail = xfail
        self.pages = tuple(pages) if pages else ()
        self.name = fn.__name__
        self.module = fn.__module__.rsplit(".", 1)[-1]
        self.doc = (fn.__doc__ or "").strip().split("\n")[0]


def test(*rules, where="both", slow=False, needs=None, smoke=False, xfail=None, pages=None, sim_args=None):
    """Register a test: the rule IDs it checks; where it runs (both, radio, sim); slow (waits on timers);
    needs: the build flavours it needs (("tx", "tx-dry") for a transmit build's screens), else any, and the CAPS
    it needs (("bench",), ("keyup",), ("bt",), ...); smoke: in
    the short set bench.py runs beside the SDR suite; xfail: a known deviation from the rule (the reason): a
    failure is reported as xfail (not a run failure, recorded ✗), a pass as xpass (fixed: drop the mark);
    pages: the screens it visits, by readback id ("Home", "Quick", "Zones"), for --page; sim_args: the simulator's
    command-line flags for the state it needs (--sim auto starts one with them for this test alone)."""
    def deco(fn):
        REGISTRY.append(Test(fn, rules, where, slow, needs, smoke, xfail, pages, sim_args))
        return fn
    return deco


class U:
    """One readback of the screen (ferrite_link.parse_ui's dict) with short names."""

    def __init__(self, d):
        self.d = d
        sc = d.get("screen", {})
        self.id, self.name, self.title = sc.get("id", ""), sc.get("name", ""), sc.get("title", "")
        self.rows, self.sel, self.hints = d["rows"], d["sel"], d["hints"]
        self.toast, self.popup, self.qmenu = d["toast"], d["popup"], d["qmenu"]
        self.bar, self.state = d.get("bar", {}), d.get("state", {})
        self.cards = {c["slot"]: c for c in d["cards"]}
        self.bcast = d.get("bcast", {})  # a broadcast card's report by slot: rds, stereo, dbuv, snr (BCR-26)
        self.lines = d["lines"]

    def __getitem__(self, k):
        return self.d.get(k)

    @property
    def whole(self):
        """The readback ran to its end: the hints line is always the last (uistate.rs)."""
        return bool(self.lines) and self.lines[0].startswith("screen ") and self.lines[-1].split(" ", 1)[0] == "hints"

    @property
    def settled(self):
        """What a key changes and a test reads at once: the screen line, toast or popup, quick menu, hint bar."""
        return tuple(ln for ln in self.lines if ln.split(" ", 1)[0] in ("screen", "toast", "popup", "qmenu", "hints"))

    def row(self, name):
        for r in self.rows:
            if r["name"] == name:
                return r
        return None

    def num(self, n):
        for r in self.rows:
            if r["num"] == n:
                return r
        return None

    def names(self):
        return [r["name"] for r in self.rows]

    @property
    def active(self):
        return self.cards.get(self.state.get("active", "A"), {})

    def hint_keys(self):
        return [k for k, _ in self.hints]

    def hint(self, key):
        for k, v in self.hints:
            if k == key:
                return v
        return None

    def text(self):
        return "\n".join(self.lines)


def mhz_of(s):
    return float(s) if s not in (None, "") else None


GET = re.compile(r"^(\d(?: \d)*) ([^:]+?): (.*?)( \(not the default\))?\s*$")


class T:
    """The context a test gets: the radio, key presses, the readback, checks with evidence, settings."""

    def __init__(self, radio, sim, build, log=print):
        self.r, self.sim, self.build, self.log = radio, sim, build, log
        self.opts = None  # the run's options (uitest.py's argparse namespace)
        self._bt = None
        self.checks, self.notes, self.last = [], [], None
        self.evidence = []  # the readbacks a check looked at (their lines), newest last
        self.visited = set()  # the screens read back (--check-pages), outside home()
        self._homing = False
        self._rb_s = 0.03  # how long a readback takes (a running average; `ui` starts that much early)

    # -- keys and readback --
    def ui(self, settle=0.12):
        """The screen read back `settle` s after the last key, once it has settled (`stable`). The first of
        `stable`'s readbacks starts one readback's time early, so the one returned is taken no sooner than before
        and a settled screen costs no extra time."""
        if settle:
            time.sleep(max(0.0, settle - self._rb_s))
        self.last = self.stable()
        if not self._homing and self.last.id:
            self.visited.add(self.last.id)
        return self.last

    def stable(self, timeout=STABLE_S):
        """A readback that is whole and settled: read until two in a row agree on the screen, its toast or popup,
        the quick menu and the hint bar (`U.settled`), each whole (`U.whole`), or `timeout` s pass (then the
        last). The radio describes the screen afresh for each page of an answer and takes remote keys on its next
        tick, so one readback can show the previous screen, or end early (a toast going between pages drops the
        last line, the hints) or mix two screens; the second readback is the only cost when nothing moves."""
        end = time.time() + timeout
        prev = None
        while True:
            t0 = time.time()
            u = U(self.r.ui())
            self._rb_s = 0.8 * self._rb_s + 0.2 * min(time.time() - t0, 0.1)
            if u.whole and prev is not None and u.settled == prev.settled:
                return u
            if time.time() > end:
                return u
            if prev is not None or not u.whole:
                time.sleep(0.02)  # it is moving: let it get on
            prev = u if u.whole else None

    def keys(self, seq, settle=0.12):
        self.r.keys(seq)
        return self.ui(settle)

    def press(self, name, long=False, settle=0.12):
        self.r.press(name, long=long)
        return self.ui(settle)

    def knob(self, n, settle=0.12):
        self.r.knob(n)
        return self.ui(settle)

    def wait(self, pred, timeout=3.0, what="the screen", every=0.15):
        end = time.time() + timeout
        while True:
            u = self.ui(every)
            if pred(u):
                return u
            if time.time() > end:
                self.fail(f"{what}: not within {timeout:g} s")

    def home(self):
        """Home, unlocked, no quick menu: hold BACK (HOME-2), then BACK x4 if that wasn't enough. Its readbacks
        aren't pages the test visits."""
        homing, self._homing = self._homing, True
        try:
            return self._home()
        finally:
            self._homing = homing

    def _home(self):
        u = self.ui(0)
        pin = getattr(self.opts, "pin", None)
        if u.id == "Pin" and pin:
            # A test that restarted the radio with a PIN set leaves it on PIN entry (Oct 15, 2026: every later test
            # failed "can't get home (on Pin)"): unlock with the run's --pin.
            self.r.keys(" ".join(pin))
            u = self.press("OK")
        if u.bar.get("lock"):
            self.r.press("HASH", long=True)
        for _ in range(2):
            u = self.press("BACK", long=True)
            if u.id in HOMES and not u.qmenu:
                return u
        for _ in range(4):
            u = self.press("BACK")
        if u.id not in HOMES:
            self.fail(f"can't get home (on {u.id})")
        return u

    def unmute(self):
        """The active slot unmuted (HOME-15's quick mute isn't in `slot`): True if it was muted (put it back with
        STAR on home)."""
        u = self.ui(0)
        if "muted" in str(u.active.get("state", "")).lower():
            u = self.press("STAR")  # HOME-15.1: one STAR unmutes; Off only if it lands within 1 s of the muting one
            if str(u.active.get("state", "")) == "Off":
                self.press("STAR")
            return True
        return False

    def stop_scans(self, keep=()):
        """Stop the scans running on the slots (but those in `keep`), so their audio and their scan stops leave
        the other cards alone (the simulator's sample scans C). Returns the slots stopped: give them to
        `restart_scans` at the end (the runner's restore doesn't compare "scanning")."""
        stopped = []
        active = self.ui(0).state.get("active", "A")
        for L in "ABC":
            if L in keep or "scanning" not in self.slot(L):
                continue
            self.cmd(f"slot {L} active")
            self.home()
            self.press("SIDE2", settle=0.3)
            self.home()
            if "scanning" not in self.slot(L):
                stopped.append(L)
        self.cmd(f"slot {active} active")
        self.home()
        return stopped

    def restart_scans(self, slots):
        """SIDE2 on each slot `stop_scans` stopped, then the active slot as it was."""
        if not slots:
            return
        active = self.ui(0).state.get("active", "A")
        for L in slots:
            self.cmd(f"slot {L} active")
            self.home()
            self.press("SIDE2", settle=0.3)
            self.home()
        self.cmd(f"slot {active} active")
        self.home()

    def put_back(self, stopped):
        """The slots as at the run's start (the runner's snapshot), then the scans `stop_scans` stopped."""
        notes = []
        self.home()
        self.state.restore_slots(notes)
        self.restart_scans(stopped)
        for n in notes:
            self.note(n)

    def screen(self, want, u=None):
        u = u or self.last or self.ui()
        self.eq(u.id, want, "the screen")
        return u

    # -- checks --
    def check(self, cond, what, got=None):
        self.checks.append(dict(what=what, ok=bool(cond), got=None if got is None else str(got)[:300]))
        if self.last is not None and (not self.evidence or self.evidence[-1] is not self.last.lines):
            self.evidence.append(self.last.lines)
            del self.evidence[:-6]
        if not cond:
            raise Fail(f"{what}" + (f": got {str(got)[:300]}" if got is not None else ""))

    def eq(self, got, want, what):
        self.check(got == want, f"{what} is {want!r}", got)

    def has(self, hay, needle, what):
        self.check(needle in (hay or ""), f"{what} has {needle!r}", hay)

    def fail(self, what):
        self.check(False, what)

    def note(self, s):
        self.notes.append(s)

    # -- terminal commands and settings --
    def cmd(self, line):
        return self.r.cmd(line)

    def cmd_err(self, line):
        """(ok, text): the output, or the radio's refusal reason."""
        from ferrite_link import RadioError
        try:
            return True, self.r.cmd(line)
        except RadioError as e:
            return False, str(e)

    def get(self, path):
        out = self.cmd(f"get {path}").strip()
        m = GET.match(out)
        if not m:
            self.fail(f"get {path}: can't read {out!r}")
        return m.group(3)

    def get_full(self, path):
        """(name, value, non-default)."""
        m = GET.match(self.cmd(f"get {path}").strip())
        return m.group(2), m.group(3), bool(m.group(4))

    def set(self, path, value):
        if path in UNSAFE:
            raise Fail(f"{path} is never changed by a test")
        ok, out = self.cmd_err(f"set {path} {value}")
        if not ok:
            self.fail(f"set {path} {value}: {out}")

    def slot(self, letter):
        return self.cmd(f"slot {letter}").strip()

    def need(self, cond, why):
        """A precondition the radio's state doesn't meet: the test is skipped, not failed."""
        if not cond:
            raise Skip(why)

    # -- restarts, Bluetooth, the bench commands, remote key-ups, owner settings --
    def restart(self, capture=0.0):
        """Restart the radio over the link (REBOOT, LINK-24: into the stock bootloader, which starts Ferrite again
        after its 200 ms listen) and connect again. `capture` s: the cable's boot report read first, without a
        byte sent (radio only; the simulator returns ""). Returns the readback once home or setup shows."""
        from ferrite_link import REBOOT
        self.r._ok(REBOOT)
        text = self.r.reopen(capture=0 if self.sim else capture)
        self.boot_text = text
        u = self.wait(lambda u: u.id not in ("", "Boot") and u.whole, timeout=15, what="the radio up again")
        return u

    def resync(self, timeout=30.0, what="the radio answering again"):
        """After a restart (a REBOOT, the factory reset): the readback polled until a whole screen past boot comes
        back. Just after a restart the cable's bytes go to the terminal until a 0x00 sync (TERM-2) and a CMD is
        never sent twice by the link, so this goes first; if the readback doesn't answer, the port is opened again
        (HELLO until it answers). Returns the readback."""
        from ferrite_link import RadioError
        end = time.time() + timeout
        reopened, last = False, None
        while True:
            try:
                u = U(self.r.ui())
                if u.id not in ("", "Boot") and u.whole:
                    return self.ui()
                last = f"screen {u.id or '?'}"
            except (TimeoutError, OSError, RadioError) as e:
                last = e
                if not reopened and hasattr(self.r, "reopen"):
                    reopened = True
                    try:
                        self.r.reopen(settle=1.0, timeout=max(2.0, end - time.time()))
                    except (TimeoutError, OSError) as e2:
                        last = e2
                    continue
            if time.time() > end:
                self.fail(f"{what}: not within {timeout:g} s ({last})")
            time.sleep(0.5)

    def bt_raw_again(self, timeout=20.0):
        """A new Bluetooth link (bt_raw) after the radio restarted: the connection dropped with the restart and the
        module advertises again, so it's tried every 2 s for `timeout` s."""
        self.close_bt()
        end, last = time.time() + timeout, None
        while True:
            try:
                self._bt = BtRaw(self.opts.bt)
                return self._bt
            except (TimeoutError, OSError) as e:
                last = e
            if time.time() > end:
                self.fail(f"Bluetooth again after the restart: no connection in {timeout:g} s ({last})")
            time.sleep(2.0)

    def bt(self):
        """A second link on Bluetooth (--bt; the simulator's own with --sim auto), past HELLO; closed after the
        test. Bluetooth must be on (0 9 2 1): the simulator turns it on for the test; on the radio it's the owner's."""
        if self._bt is None:
            import ferrite_link as fl
            spec = getattr(self.opts, "bt", None)
            self.need(spec, "no --bt link")
            if self.get("0 9 2 1") != "On":
                self.need(self.sim, "Bluetooth is off (0 9 2 1): the owner turns it on")
                self.owner_set("0 9 2 1", "On")
            self.need(self.get("0 9 3 1") == "Off", "a PIN lock is on: Bluetooth asks for the PIN (bt_raw)")
            self._bt = fl.Radio(spec, name=b"uitest.py bt")
        return self._bt

    def bt_raw(self):
        """A Bluetooth link without HELLO's AUTH (a PIN lock may be on): `.link` to ask what a test wants."""
        self.need(getattr(self.opts, "bt", None), "no --bt link")
        if self.get("0 9 2 1") != "On":
            self.need(self.sim, "Bluetooth is off (0 9 2 1): the owner turns it on")
            self.owner_set("0 9 2 1", "On")
        self.close_bt()
        self._bt = BtRaw(self.opts.bt)
        return self._bt

    def bt_port(self):
        """Bluetooth's serial side bare (no HELLO: the terminal is Enter's), as a phone's terminal app has it."""
        import ferrite_link as fl
        self.need(getattr(self.opts, "bt", None), "no --bt link")
        if self.get("0 9 2 1") != "On":
            self.need(self.sim, "Bluetooth is off (0 9 2 1): the owner turns it on")
            self.owner_set("0 9 2 1", "On")
        self.close_bt()
        self._bt = fl.Port(self.opts.bt)
        self._bt.buf = getattr(self._bt, "buf", bytearray())
        return self._bt

    def close_bt(self):
        """The Bluetooth link closed (a connection ends; the next bt() is a new one)."""
        if self._bt is not None:
            try:
                self._bt.close()
            except OSError:
                pass
            self._bt = None

    def cable_log(self):
        """The cable log (0 9 9 5, TERM-28) read from --cablelog without a byte sent (the first byte would end it),
        while the keys and readback go over Bluetooth (--port ble:...): turned on for the test, off after."""
        self.need(getattr(self.opts, "cablelog", None), "no --cablelog PORT")
        if getattr(self, "_log", None) is None:
            self._log = CableLog(self.opts.cablelog)
            # Off, then On: a byte on the cable ends the log (TERM-28) while the setting stays On, and setting On
            # again then changes nothing, so the log stayed silent (cablelog.gps_quiet_standby, Oct 15, 2026).
            self.owner_set("0 9 9 5", "Off")
            time.sleep(0.3)
            t0 = time.time()
            self.owner_set("0 9 9 5", "On")
            # The receiver is logged once a second: a quiet log after 3 s is the cable, not the test's subject.
            self.need(self._log.wait(r".", t0, 3.0), f"no cable log lines on {self.opts.cablelog} within 3 s of "
                      "0 9 9 5 On: is the cable on that port at 115200 and nothing else writing to it?")
        return self._log

    def end_test(self):
        """After a test (the runner): Bluetooth and the cable log closed, the owner settings it changed put back."""
        if getattr(self, "_log", None) is not None:
            self._log.close()
            self._log = None
        self.close_bt()
        for path, value in reversed(getattr(self, "_owner", [])):
            try:
                self.cmd(f"set {path} {value}")
            except Exception as e:  # noqa: BLE001 (the runner's restore reports what is left)
                self.note(f"{path}: couldn't put back {value!r}: {e}")
        self._owner = []

    def owner_set(self, path, value):
        """A setting UNSAFE keeps from tests (Bluetooth, the PIN lock), changed where the test needs it and put back
        after it whatever happens (the cable is never asked for the PIN: TERM-3). Only the settings named here."""
        allowed = {"0 9 2 1": self.sim, "0 9 3 1": True, "0 9 3 3": True, "0 9 9 5": True}
        if not allowed.get(path):
            raise Fail(f"{path} is never changed by a test here")
        if not hasattr(self, "_owner"):
            self._owner = []
        was = self.get(path)
        if not any(p == path for p, _ in self._owner):
            self._owner.append((path, was))
        ok, out = self.cmd_err(f"set {path} {value}")
        if not ok:
            self.fail(f"set {path} {value}: {out}")

    def bench(self):
        self.need(self.r.bench and not self.sim, "not a bench build: bk, gpio, mem are in tx-dry and bring-up")

    def bk(self, chip, reg):
        """A BK4829 register by `bk` (bench builds): its value."""
        out = self.cmd(f"bk {chip} {reg:x}")
        m = re.search(r"REG_[0-9A-Fa-f]+ = 0x([0-9A-Fa-f]{4})", out)
        if not m:
            self.fail(f"bk {chip} {reg:x}: {out!r}")
        return int(m.group(1), 16)

    def gpio(self):
        """`gpio` (bench builds): {"A": (in, out), ...} as 16-bit numbers."""
        out = self.cmd("gpio")
        ports = {}
        for m in re.finditer(r"P([A-E])\s+in ([01 ]{19})\s+out ([01 ]{19})", out):
            ports[m.group(1)] = (int(m.group(2).replace(" ", ""), 2), int(m.group(3).replace(" ", ""), 2))
        if len(ports) != 5:
            self.fail(f"gpio: {out!r}")
        return ports

    def pin(self, name, which=1):
        """A pin's output (which=1) or input (which=0) level by `gpio`: pin("PC4")."""
        return self.gpio()[name[1]][which] >> int(name[2:]) & 1

    def mem32(self, addr, n=1):
        out = self.cmd(f"mem32 {addr:08X} {n}")
        words = []
        for line in out.splitlines():
            parts = line.split()
            if parts and re.fullmatch(r"[0-9A-Fa-f]{8}", parts[0]):
                words += [int(w, 16) for w in parts[1:] if re.fullmatch(r"[0-9A-Fa-f]{8}", w)]
        if len(words) < n:
            self.fail(f"mem32 {addr:08X} {n}: {out!r}")
        return words[:n]

    def keyup_refused(self, slot="active"):
        """A remote key-up that must be refused (LINK-20): only with Dry TX on (PA held at 0) or the simulator.
        KEY on, then KEY off at once whatever came back; (refused, the reason). A key-up that keyed fails the
        test after the KEY off."""
        import ferrite_link as fl
        if not (self.sim or self.build in DRY):
            raise Fail("a remote key-up only with Dry TX on (tx-bench at power-on) or the simulator")
        if not self.sim:
            # The bench's keying frequencies only (tools/bench.py test_freqs), simplex, whatever the check expects.
            line = self.slot(self.ui(0).state.get("active", "A") if slot == "active" else slot.upper())
            m = re.search(r" (\d+\.\d+)\s", line + " ")
            if not m or m.group(1) not in ("145.49500", "446.47500") or "split" in line or (
                    "offset=" in line and "offset=off" not in line and "offset=+0.00000" not in line):
                raise Fail(f"a remote key-up on the radio only on 145.495 or 446.475 MHz, simplex: {line!r}")
        link = self.r.link
        status, rest = None, b""
        try:
            status, rest = link.ask(fl.KEY, bytes([fl.PTT, fl.PTT_ON, fl.SLOTS[slot]]), timeout=1.0)
        finally:
            for _ in range(3):
                try:
                    link.ask(fl.KEY, bytes([fl.PTT, fl.PTT_OFF]), timeout=1.0)
                    break
                except TimeoutError:
                    continue
        if status == 0:
            self.fail("the remote key-up keyed (KEY off sent at once)")
        return True, rest.decode("ascii", "replace")


class BtRaw:
    """A Bluetooth link past HELLO only (no AUTH): what a phone would see with a PIN lock on."""

    def __init__(self, spec):
        import ferrite_link as fl
        self.port = fl.Port(spec)
        self.link = fl.Link(self.port)
        try:
            self.hello = self.link.ask(fl.HELLO, bytes([fl.PROTOCOL]) + bytes(8) + b"uitest.py bt")
        except BaseException:
            self.close()
            raise

    def close(self):
        self.port.close()


class CableLog:
    """The cable's log lines as they come (a reader thread; nothing is ever written to the port): (time, line)."""

    def __init__(self, port):
        import threading
        import serial
        self.ser = serial.Serial(port, 115200, timeout=0.1)
        self.lines, self._buf, self._stop = [], b"", False
        self._t = threading.Thread(target=self._run, daemon=True)
        self._t.start()

    def _run(self):
        while not self._stop:
            b = self.ser.read(4096)
            if not b:
                continue
            self._buf += b
            while b"\n" in self._buf:
                line, _, self._buf = self._buf.partition(b"\n")
                self.lines.append((time.time(), line.decode("ascii", "replace").rstrip("\r")))

    def since(self, t0):
        return [(ts, ln) for ts, ln in list(self.lines) if ts >= t0]

    def wait(self, pattern, t0, timeout):
        """The first line from t0 on matching the regex, within `timeout` s, or None."""
        end = time.time() + timeout
        while True:
            for ts, ln in self.since(t0):
                if re.search(pattern, ln):
                    return ts, ln
            if time.time() > end:
                return None
            time.sleep(0.1)

    def close(self):
        self._stop = True
        self._t.join(timeout=1)
        self.ser.close()
