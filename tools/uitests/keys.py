"""Keys, the key lock, hints and the remote keys themselves (chapter 7, KEY-*; LINK-19)."""
import time

from .core import test

# Screens to walk for the hint bar and the status bar's name (keys from home).
WALK = ["MENU", "MENU 0", "MENU 0 1", "MENU 0 7", "MENU 0 7 5", "MENU 0 9 1", "MENU 0 0", "MENU 0 9 8",
        "MENU 0 9 8 RIGHT", "MENU 0 9 8 RIGHT RIGHT", "MENU 0 9 8 RIGHT RIGHT RIGHT", "MENU 0 9 1 3", "1 4",
        "SLOT:long", "MENU 0 2 6", "MENU 0 3 4", "MENU 0 6 6 MENU", "MENU 4 MENU", "MENU MENU", "9:long",
        "1:long", "SIDE2:long", "VM:long"]


@test("KEY-9", "SBAR-5", "MENU-16", pages=("More", "Counter", "Crossband", "Edit", "Entry", "Group", "Home", "Info", "Quick", "Save", "Scope", "Shortcut", "Status", "Text", "Tones", "Value", "Zones"))
def hint_bars_and_names_fit(t):
    """Every screen walked: at most three hint pairs, the universal BACK last when shown; the status bar's
    screen name at most 8 characters (none on home); lists' hint bars name the knob."""
    u = t.ui()
    t.eq(u.title, "", "home has no screen name")
    seen = []
    for seq in WALK:
        t.home()
        u = t.keys(seq, settle=0.25)
        if not u.hints:  # a hold digit's naming popup hides the hint bar for a moment (the radio, not the sim)
            u = t.wait(lambda u: u.hints, timeout=3, what=f"{seq}: the hint bar", every=0.1)
        seen.append(f"{u.id}:{u.title}")
        t.check(1 <= len(u.hints) <= 3, f"{seq}: one to three hint pairs", u.hints)
        keys = u.hint_keys()
        if "BACK" in keys:
            t.eq(keys.index("BACK"), len(keys) - 1, f"{seq}: BACK comes last")
        t.check(len(u.title) <= 8, f"{seq}: the screen name fits 8 characters", u.title)
        if u.id == "Group":
            t.eq(u.hint("Knob"), "Change", f"{seq}: a group's hint bar names the knob")
    t.note("walked " + ", ".join(seen))


@test("KEY-5", pages=("More", "Entry", "Home", "Quick"))
def back_is_one_level(t):
    """A short BACK goes back one level (on home it does nothing); in entry it deletes a digit first."""
    u = t.keys("BACK")
    t.eq(u.id, "Home", "BACK on home")
    u = t.keys("MENU 0")
    t.eq(u.id, "More", "quick, then 0")
    u = t.keys("BACK")
    t.eq(u.id, "Quick", "BACK: back to quick settings (where it was opened from)")
    u = t.keys("BACK")
    t.eq(u.id, "Home", "BACK again: home")
    u = t.keys("1 4 6")
    t.eq(u["entry"]["args"][0], "146", "entry")
    u = t.keys("BACK")
    t.eq((u.id, u["entry"]["args"][0]), ("Entry", "14"), "BACK deletes a digit first")
    u = t.keys("BACK BACK")
    t.check(u.id in ("Entry", "Home"), "BACK past the digits", u.id)
    u = t.keys("BACK")
    t.eq(u.id, "Home", "and then home")


@test("SQL-11", "KEY-14.1", pages=("Home",))
def monitor_while_locked(t):
    """Locked, SIDE1 still opens the monitor (and again closes it)."""
    was_muted = t.unmute()
    t.press("HASH", long=True)
    try:
        u = t.keys("SIDE1")
        t.check("MON" in u.active["state"] or (u.toast and u.toast["head"] == "Monitor"), "SIDE1 monitors while locked",
                (u.active["state"], u.toast))
        u = t.keys("SIDE1")
        t.check("MON" not in u.active["state"], "SIDE1 again: monitor off", u.active["state"])
    finally:
        t.press("HASH", long=True)
        if was_muted:
            t.home()
            t.keys("STAR")


@test("KEY-13", "KEY-14.4", "SBAR-11", smoke=True, pages=("Home", "Quick"))
def key_lock(t):
    """Hold # locks (padlock in the bar, popup); locked, keys and the knob give "Locked: hold # to unlock" and
    do nothing, hold * gives no quick menu; hold # unlocks."""
    was_muted = t.unmute()
    f0 = t.ui().active["freq"]
    u = t.press("HASH", long=True)
    t.eq(u.bar["lock"], 1, "the padlock")
    t.eq(u.state["locked"], 1, "locked")
    t.check(u.popup and u.popup["head"] == "Keypad locked", "the popup", u.popup)
    t.eq(u.hints, [("#", "Hold: unlock")], "the hint bar")
    for k in ("MENU", "5", "SLOT", "UP"):
        u = t.keys(k)
        t.eq(u.id, "Home", f"{k} locked: still home")
        t.check(u.toast and u.toast["head"].startswith("Locked"), f"{k}: the locked toast", u.toast)
    t.eq(u.state["active"], "A", "SLOT did nothing")
    u = t.knob(+2)
    t.eq(u.active["freq"], f0, "the knob doesn't tune")
    t.check(u.toast and u.toast["head"].startswith("Locked"), "the knob: the locked toast", u.toast)
    u = t.press("STAR", long=True)
    t.eq(u.qmenu, [], "hold * locked: no quick menu")
    u = t.press("HASH", long=True)
    t.eq((u.bar["lock"], u.state["locked"]), (0, 0), "hold # unlocks")
    u = t.keys("MENU")
    t.eq(u.id, "Quick", "keys work again")
    if was_muted:
        t.home()
        t.keys("STAR")


@test("KEY-14.1", "KEY-14.2", "KEY-14.3", "KEY-13", pages=("Home",))
def lock_modes(t):
    """Hold # locks the keypad and its popup offers * (All) and 0 (All but PTT): Keypad lets SIDE2 scan; All but
    PTT locks SIDE2 too; * gives All. The popup names each, as the bar's lockmode does."""
    t.home()
    try:
        u = t.press("HASH", long=True)
        t.check(u.popup and u.popup["head"] == "Keypad locked", "the keypad's popup", u.popup)
        t.eq(u.bar.get("lockmode"), "keypad", "bar lockmode")
        t.wait(lambda u: not u.popup, timeout=5, what="the popup gone")
        u = t.keys("SIDE2")
        t.check(not (u.toast and u.toast["head"].startswith("Locked")), "keypad lock: SIDE2 works", u.toast)
        t.keys("SIDE2")
        t.press("HASH", long=True)
        t.press("HASH", long=True)
        u = t.keys("0")
        t.check(u.popup and u.popup["head"] == "All keys locked" and u.popup["body"] == "PTT works",
                "0: all but PTT's popup", u.popup)
        t.eq(u.bar.get("lockmode"), "allbutptt", "bar lockmode")
        u = t.keys("SIDE2")
        t.check(u.toast and u.toast["head"].startswith("Locked"), "all but PTT: SIDE2 locked", u.toast)
        t.press("HASH", long=True)
        t.press("HASH", long=True)
        u = t.keys("STAR")
        t.check(u.popup and u.popup["body"] == "PTT too", "*: all's popup", u.popup)
        t.eq(u.bar.get("lockmode"), "all", "bar lockmode")
    finally:
        if t.ui().bar.get("lock") == 1:
            t.press("HASH", long=True)


@test("KEY-20", "KEY-15", slow=True, pages=("Home",))
def auto_lock(t):
    """Auto lock 30 s (0 9 1 1): with no key for 30 s the keys lock and the padlock shows."""
    t.set("0 9 1 1", "30 s")
    t.home()
    u = t.wait(lambda u: u.bar.get("lock") == 1, timeout=36, what="locked after 30 s", every=1.0)
    t.eq(u.state["locked"], 1, "locked")
    t.press("HASH", long=True)
    t.eq(t.ui().bar["lock"], 0, "hold # unlocks")


@test("KEY-11", "KEY-21", "MENU-14", "VFO-26", pages=("Value", "Entry", "Home"))
def digit_shortcuts_on_a_channel(t):
    """Hold 6 on a channel changes nothing and says VFO only; hold 7 opens the shift's options and hold 8 the
    offset's amount, on a channel as on a VFO."""
    t.need("ch " in t.slot("B"), "slot B isn't on a channel")
    t.cmd("slot B active")
    u = t.press("6", long=True)
    t.check(u.popup and "VFO only" in u.popup["head"] + " " + u.popup["body"], "hold 6: VFO only", u.popup)
    for d, want in (("7", "Value"), ("8", "Entry")):
        t.home()
        u = t.press(d, long=True)
        t.eq(u.id, want, f"hold {d} on a channel")
    t.home()
    t.cmd("slot A active")


@test("KEY-21", "HOME-8", pages=("Home", "Shortcut"))
def shortcut_list_runs_a_row(t):
    """MENU 0 9 1 3: the shortcut list; a digit picks a row and OK runs it from home with its popup."""
    u = t.keys("MENU 0 9 1 3")
    t.eq(u.id, "Shortcut", "MENU 0 9 1 3")
    p0 = t.slot("A")
    u = t.keys("4 MENU")
    t.eq(u.id, "Home", "OK runs it from home")
    t.check(u.popup and u.popup["head"] == "Watch", "with its popup", u.popup)
    t.press("4", long=True)
    t.eq(t.slot("A"), p0, "slot unchanged")


@test("KEY-22", "NAV-3", "MENU-35", pages=("Counter", "Home"))
def side2_counter_and_big_steps(t):
    """SIDE2 alone opens the counter. The knob sets RF gain and the arrows make big steps."""
    u = t.press("SIDE2", long=True)
    t.eq(u.id, "Counter", "hold SIDE2 alone")
    t.home()
    f0 = float(t.ui().active["freq"])
    t.r.down("SIDE2")
    time.sleep(0.15)
    t.r.knob(+1)
    time.sleep(0.9)
    t.r.up("SIDE2")
    u = t.ui(0.2)
    t.eq(u.id, "Home", "SIDE2 + knob: no counter")
    t.eq(float(u.active["freq"]), f0, "SIDE2 + knob keeps the frequency")
    t.r.down("SIDE2")
    time.sleep(0.15)
    t.r.knob(-4)
    u = t.press("UP")
    t.check(abs(float(u.active["freq"]) - f0 - 1.0) < 1e-6, "SIDE2 + UP: 1 MHz", u.active["freq"])
    t.press("DOWN")
    t.r.up("SIDE2")
    t.eq(float(t.ui(0.2).active["freq"]), f0, "SIDE2 + DOWN: back to the original frequency")


@test("LINK-19", "KEY-3", "KEY-6", smoke=True, pages=("More", "Entry", "Home", "Quick"))
def remote_keys_and_readback(t):
    """The link's keys: every key name answers (`ui keys`), short and long presses and down/up apart reach the
    screens as the keypad's do; keys typed fast are all taken."""
    names = t.r.ui_text(1)
    for k in ("MENU", "BACK", "UP", "DOWN", "LEFT", "RIGHT", "SLOT", "VM", "STAR", "HASH", "SIDE1", "SIDE2"):
        t.check(f"\n{k} " in "\n" + names, f"`ui keys` lists {k}", names[:200])
    t.check(all(f"\n{d} " in "\n" + names for d in "0123456789"), "the digits", names)
    f0 = t.ui().state["frame"]
    u = t.keys("MENU")
    t.check(u.state["frame"] > f0, "a key redraws", u.state)
    t.home()
    t.r.keys("1 4 6 STAR 9 4")  # typed at full speed: none dropped
    u = t.ui(0.3)
    t.eq((u.id, u["entry"]["args"][0] if u["entry"] else None), ("Entry", "146.94"), "every key typed ahead")
    t.home()
    t.r.down("MENU")
    time.sleep(0.9)
    t.r.up("MENU")
    t.eq(t.ui().id, "More", "MENU down and up 0.9 s apart: a long press")
