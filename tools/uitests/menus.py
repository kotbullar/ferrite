"""Quick settings, advanced settings and the groups (manual chapter 11, MENU-*)."""
import re

from .core import test

QUICK_ROWS = ["Squelch", "TX power", "Mode", "Tones", "Offset", "Step", "Watch", "Channel", "Battery Save",
              "RF gain", "More"]
GROUPS = ["Watch and scan", "Channels", "Tools", "Decoders and alerts", "APRS", "GPS and time",
          "Display, sound, power", "Transmit", "System"]
CHIP_WORDS = re.compile(r"BK4829|SI4732|BT513|W25Q|AT32|ST7789", re.I)


@test("MENU-1", "MENU-12", "MENU-35", "KEY-9", smoke=True, pages=("More", "Quick"))
def quick_settings_rows(t):
    """MENU on home opens the numbered rows, the PF2 RF gain row and More."""
    u = t.keys("MENU")
    t.eq(u.id, "Quick", "MENU on home opens")
    t.eq(u.names(), QUICK_ROWS, "the rows, in order")
    t.eq([r["num"] for r in u.rows], [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 0], "the readback row numbers")
    t.check(all(r["value"] for r in u.rows), "every row shows a value", u.rows)
    t.eq(u.sel and u.sel["name"], "Squelch", "the first row selected")
    t.check(len(u.hints) <= 3, "at most three hint pairs", u.hints)
    u = t.press("SIDE2")
    t.eq(u.sel and u.sel["name"], "RF gain", "PF2 selects RF gain")
    t.press("HASH")
    u = t.knob(1)
    t.eq(u.sel and u.sel["value"], "1 / 4", "one knob step")
    u = t.press("HASH")
    t.eq(u.sel and u.sel["value"], "Auto", "# resets RF gain")
    u = t.keys("0")
    t.eq(u.id, "More", "0 in quick settings opens")


@test("MENU-2", "MENU-4", "SBAR-13", smoke=True, pages=("More", "Status"))
def advanced_groups(t):
    """MENU 0 and hold MENU on home: More, Recent and the nine task-named groups; MENU 0 0 is Status."""
    u = t.keys("MENU 0")
    t.eq(u.id, "More", "MENU 0")
    t.check(u["recent"] is not None, "a Recent line", u.lines[:6])
    t.eq(u.names(), GROUPS, "the nine groups")
    t.eq([r["num"] for r in u.rows], list(range(1, 10)), "numbered 1-9")
    t.check(not any(CHIP_WORDS.search(n) for n in u.names()), "no group named by a chip", u.names())
    t.home()
    u = t.press("MENU", long=True)
    t.eq(u.id, "More", "hold MENU on home")
    t.home()
    u = t.keys("MENU 0 0")
    t.eq(u.id, "Status", "MENU 0 0")


@test("MENU-3", "MENU-4", "MENU-12", "MENU-15", pages=("Group",))
def every_group_fits_and_names_its_path(t):
    """Each group: at most nine numbered rows, a help line with its MENU path, no chip names."""
    for g in range(1, 10):
        t.home()
        u = t.keys(f"MENU 0 {g}")
        t.eq(u.id, "Group", f"MENU 0 {g}")
        t.check(1 <= len(u.rows) <= 9, f"group {g}: 1-9 rows", len(u.rows))
        t.eq([r["num"] for r in u.rows], list(range(1, len(u.rows) + 1)), f"group {g}: rows numbered")
        t.check(all(r["name"] for r in u.rows), f"group {g}: every row named", u.names())
        t.check(not any(CHIP_WORDS.search(n) for n in u.names()), f"group {g}: no chip names", u.names())
        # The group opens on row 1, or on the row it was last left on (MENU-24): the help names the selected row.
        h = u["help"]
        n = u.sel["num"] if u.sel else 1
        t.check(h and h["args"][0] == f"MENU 0 {g} {n}", f"group {g}: help names the selected row's path", h)
        u = t.keys("DOWN")
        n2 = u.sel["num"] if u.sel else 1
        t.check(u["help"]["args"][0] == f"MENU 0 {g} {n2}" and (n2 != n or len(u.rows) == 1),
                f"group {g}: help follows the row", u["help"])


@test("MENU-3", "MENU-5", pages=("Group",))
def third_level_and_no_deeper(t):
    """A group's third level (0 1 6, Scan's Advanced) opens by its digit; a digit inside it selects, never opens."""
    u = t.keys("MENU 0 1 6")
    t.eq(u.id, "Group", "MENU 0 1 6")
    t.check(u["help"]["args"][0].startswith("MENU 0 1 6 "), "its rows are MENU 0 1 6 N", u["help"])
    u = t.keys("3")
    t.eq(u.id, "Group", "a digit on a third-level row")
    t.eq(u["help"]["args"][0], "MENU 0 1 6 3", "selects row 3 (no fourth level)")


@test("MENU-5", "MENU-6", pages=("More", "Group", "Home", "Quick"))
def fast_path_lands_where_slow_does(t):
    """MENU 0 7 1 typed at full speed and slowly lands on the same row; the same from another screen."""
    t.r.keys("MENU 0 7 1")
    fast = t.ui(0.2)
    t.home()
    for k in "MENU 0 7 1".split():
        t.press(k, settle=0.35)
    slow = t.ui()
    t.eq((fast.id, fast.title, fast.sel and fast.sel["name"]), (slow.id, slow.title, slow.sel and slow.sel["name"]),
         "fast and slow")
    t.eq(slow.sel["name"], "Theme", "MENU 0 7 1")
    # The number means the same with another slot active.
    t.home()
    t.keys("SLOT")
    u = t.keys("MENU 0 7 1")
    t.eq(u.sel and u.sel["name"], "Theme", "MENU 0 7 1 with slot B active")
    t.home()
    t.keys("SLOT SLOT")


@test("MENU-7", "MENU-12", "MENU-15", pages=("Group",))
def row_names_match_the_settings_table(t):
    """The group rows for settings carry the same name `get` gives for their number (one table)."""
    checked = 0
    for path in ("0 7 1", "0 7 2", "0 6 1", "0 6 4", "0 6 5", "0 8 3", "0 7 7 1"):
        name, value, nd = t.get_full(path)
        t.home()
        u = t.keys("MENU " + path)
        t.eq(u.sel and u.sel["name"], name, f"{path}: the row's name")
        t.eq(u.sel["value"], value, f"{path}: the row's value")
        t.eq(u.sel["dot"], nd, f"{path}: the dot marks a non-default value")
        t.eq(u["help"]["args"][0], "MENU " + path, f"{path}: the help names its path")
        checked += 1
    t.note(f"{checked} settings compared")


@test("MENU-8", "MENU-22", pages=("More", "Group", "Quick"))
def recent_and_undo(t):
    """A change: the hint bar offers * Undo and * undoes it; a kept change shows in Recent."""
    before = t.get("0 7 5 2")
    u = t.keys("MENU 0 7 5 2")
    t.eq(u.sel and u.sel["name"], "Beep volume", "MENU 0 7 5 2")
    u = t.knob(+1)
    t.check(u.sel["value"] != before, "the knob changed it", u.sel)
    t.eq(u.hint("*"), "Undo", "the hint bar offers * Undo")
    t.check(t.get("0 7 5 2") == u.sel["value"], "the change is in the settings at once", t.get("0 7 5 2"))
    u = t.keys("STAR")
    t.eq(u.sel["value"], before, "* puts the old value back")
    t.eq(t.get("0 7 5 2"), before, "and the settings have it")
    # Recent holds the slot's quick settings: TX power changed in quick settings comes first.
    t.home()
    u = t.keys("MENU 2")
    p0 = u.sel["value"]
    t.knob(+1)
    u = t.knob(-1)
    t.eq(u.sel["value"], p0, "TX power back")
    t.home()
    u = t.keys("MENU 0")
    rec = u["recent"]
    t.check(rec and rec.get("args", [None])[0] == "Power", "Recent names the setting changed first", rec)
    t.check(len(rec.get("args", [])) <= 3, "at most three", rec)
    t.eq(u.names(), GROUPS, "the lists don't reorder")


@test("MENU-14", "VFO-8", pages=("Home", "Quick"))
def step_greyed_on_a_channel(t):
    """On a channel the quick Step row is greyed and says "VFO only" when the knob tries it; on the VFO it isn't."""
    u = t.keys("MENU")
    t.check(not u.row("Step")["grey"], "Step on a VFO: not greyed", u.row("Step"))
    t.home()
    t.keys("SLOT")  # B: a channel in the simulator's sample
    t.need("ch " in t.slot("B"), "slot B isn't on a channel")
    u = t.keys("MENU")
    st = u.row("Step")
    t.check(st["grey"], "Step on a channel: greyed", st)
    u = t.keys("6")
    t.eq(u.sel and u.sel["name"], "Step", "6 selects it (still visible)")
    u = t.knob(+1)
    t.check(u.toast and "VFO only" in u.toast["head"] + " " + u.toast["body"], "the knob on it says why", u.toast)
    t.eq(u.row("Step")["value"], st["value"], "and changes nothing")
    t.home()
    t.keys("SLOT SLOT")


@test("MENU-16", "KEY-19", pages=("Group",))
def knob_and_arrows_change_live(t):
    """In a group: ▲▼ move, the knob changes the value at once, ◀▶ one step each way; hints name the knob."""
    u = t.keys("MENU 0 7 4 2")
    t.eq(u.sel and u.sel["name"], "Brightness", "MENU 0 7 4 2")
    t.eq(u.hint("Knob"), "Change", "the hint bar names the knob")
    v0 = int(u.sel["value"])
    u = t.knob(-1)
    t.eq(int(u.sel["value"]), v0 - 1, "knob -1")
    t.eq(t.get("0 7 4 2"), str(v0 - 1), "live: the setting changed with no edit step")
    u = t.keys("RIGHT")
    t.eq(int(u.sel["value"]), v0, "▶ one step")
    u = t.keys("LEFT")
    t.eq(int(u.sel["value"]), v0 - 1, "◀ one step back")
    u = t.keys("RIGHT UP")
    t.eq(u.sel["name"], "Timeout", "▲ moves up")
    u = t.keys("DOWN")
    t.eq(u.sel["name"], "Brightness", "▼ moves down")
    t.eq(int(u.sel["value"]), v0, "back as it was")


@test("MENU-17", "MENU-20", pages=("Quick", "Value"))
def value_editor_back_restores_ok_keeps(t):
    """OK on Squelch opens its editor: knob, ◀▶ and digits change it; BACK restores, OK keeps."""
    u = t.keys("MENU")
    v0 = u.row("Squelch")["value"]
    u = t.keys("MENU")
    t.eq(u.id, "Value", "OK on Squelch opens the editor")
    t.eq(u["value"]["args"], ["Squelch", v0], "the editor shows the value")
    u = t.knob(+1)
    t.eq(u["value"]["args"][1], str(int(v0) + 1), "knob +1")
    u = t.keys("RIGHT")
    t.eq(u["value"]["args"][1], str(int(v0) + 2), "▶ +1")
    u = t.keys("LEFT")
    t.eq(u["value"]["args"][1], str(int(v0) + 1), "◀ -1")
    u = t.keys("7")
    t.eq(u["value"]["args"][1], "7", "a digit types the value")
    u = t.keys("BACK")
    t.eq(u.id, "Quick", "BACK leaves the editor")
    t.eq(u.row("Squelch")["value"], v0, "BACK restored the old value")
    u = t.keys("MENU")
    alt = "5" if v0 != "5" else "6"
    u = t.keys(alt)
    u = t.keys("MENU")
    t.eq(u.id, "Quick", "OK leaves the editor")
    t.eq(u.row("Squelch")["value"], alt, "OK kept the new value")
    u = t.keys("MENU " + v0 + " MENU")
    t.eq(u.row("Squelch")["value"], v0, "put back")


@test("MENU-17", pages=("Quick", "Tones"))
def tone_list_opens(t):
    """OK on Tones opens the tone picker (a screen of its own); BACK returns to quick settings."""
    u = t.keys("MENU 4")
    t.eq(u.sel and u.sel["name"], "Tones", "row 4")
    u = t.keys("MENU")
    t.eq(u.id, "Tones", "OK on Tones opens the tone picker")
    u = t.keys("BACK")
    t.eq(u.id, "Quick", "BACK returns to quick settings")


@test("MENU-18", pages=("Group",))
def hash_resets_to_default(t):
    """# on a highlighted row resets it to its default (the dot goes)."""
    u = t.keys("MENU 0 7 4 2")
    v0 = u.sel["value"]
    # Start from the default (the radio may have it set off-default, and a step could land on the default).
    if u.sel["dot"]:
        u = t.keys("HASH")
    u = t.knob(-2)
    t.check(u.sel["dot"], "changed: the dot shows", u.sel)
    u = t.keys("HASH")
    t.check(not u.sel["dot"], "# reset it: no dot", u.sel)
    _, _, nd = t.get_full("0 7 4 2")
    t.check(not nd, "the setting is at its default", t.get("0 7 4 2"))
    t.note(f"was {v0}")


@test("MENU-19", "MENU-21", pages=("Group",))
def change_takes_effect_and_is_kept(t):
    """A change takes effect at once: Theme in the menu shows in `get` with no save step."""
    u = t.keys("MENU 0 7 1")
    v0 = u.sel["value"]
    u = t.knob(+1)
    v1 = u.sel["value"]
    t.check(v1 != v0, "the theme changed", v1)
    t.eq(t.get("0 7 1"), v1, "the settings have it at once")
    t.home()
    u = t.keys("MENU 0 7")
    t.eq(u.row("Theme")["value"], v1, "kept after leaving and coming back")
    t.home()
    t.set("0 7 1", v0)


@test("MENU-24", pages=("More", "Group", "Home", "Quick", "Status"))
def back_keeps_the_place(t):
    """BACK goes back one level keeping the place; each group remembers its last item while you are in the menu
    (going home forgets it); hold BACK goes home."""
    u = t.keys("MENU 0 7 5")
    t.eq((u.id, u.title), ("Group", "Sounds"), "MENU 0 7 5 opens the Sounds group")
    u = t.keys("BACK")
    t.eq((u.title, u.sel and u.sel["name"]), ("Display", "Sounds"), "BACK: Display, on Sounds")
    u = t.keys("BACK")
    t.eq((u.id, u.sel and u.sel["name"]), ("More", "Display, sound, power"), "BACK: More, on group 7")
    # Each group remembers its last item while you stay in the menu; going home forgets it.
    u = t.keys("7")
    t.eq((u.title, u.sel and u.sel["name"]), ("Display", "Sounds"), "7 again: Display, still on Sounds")
    u = t.press("BACK", long=True)
    t.eq(u.id, "Home", "hold BACK")
    u = t.keys("MENU 0 7")
    t.eq((u.title, u.sel and u.sel["num"]), ("Display", 1), "a new visit: row 1")
    u = t.press("BACK", long=True)
    # From the Status page: BACK returns there.
    t.keys("MENU 0 0")
    u = t.keys("DOWN DOWN DOWN DOWN DOWN MENU")
    t.check(u.id in ("Group", "Quick"), "OK on the Watch row opens its setting", u.id)
    t.eq(u.sel and u.sel["name"], "Watch", "on the Watch setting")
    u = t.keys("BACK")
    t.eq((u.id, u.sel and u.sel["name"]), ("Status", "Watch"), "BACK: Status, on Watch")


@test("MENU-25", pages=("More", "Options", "Scope", "Status"))
def hold_menu_options(t):
    """Hold MENU on another screen opens its options (numbered, 0 the full menu), or "No options here"."""
    t.keys("MENU 0 0")
    u = t.press("MENU", long=True)
    t.check(u.toast and u.toast["head"] == "No options here", "Status has no options", u.toast)
    t.home()
    u = t.press("1", long=True)  # the band scope (HOME-8)
    t.eq(u.id, "Scope", "hold 1 opens the band scope")
    u = t.press("MENU", long=True)
    t.eq(u.id, "Options", "hold MENU on the scope")
    nums = [r["num"] for r in u.rows]
    t.check(nums[-1] == 0 and u.rows[-1]["name"] == "Full menu", "0 is the full menu", u.rows)
    t.eq(nums[:-1], list(range(1, len(nums))), "numbered rows")
    u = t.keys("0")
    t.check(u.id in ("More", "Quick"), "0 opens the full menu", u.id)


@test("MENU-28", slow=True, pages=("Group", "Home"))
def idle_menu_returns_home(t):
    """An idle menu returns home after the menu timeout (0 7 9; 30 s by default, the only value for now)."""
    v = t.get("0 7 9")
    secs = int(v.split()[0]) if v.split()[0].isdigit() else 30
    u = t.keys("MENU 0 7")
    t.eq(u.id, "Group", "a menu")
    u = t.wait(lambda u: u.id == "Home", timeout=secs + 5, what=f"home after {secs} s idle", every=1.0)
    t.eq(u.id, "Home", "home")


@test("MENU-29", pages=("Group",))
def menu_never_stops_the_radio(t):
    """With the menu up, a scanning slot keeps scanning (slot line) and the cards' receivers keep running."""
    import time
    scanning = [L for L in "ABC" if "scanning" in t.slot(L)]
    started = False
    if not scanning:  # run alone (uitest -j): start a scan on the active slot (SIDE2, MENU-11), stop it after
        t.home()
        t.keys("SIDE2")
        time.sleep(0.5)
        scanning = [L for L in "ABC" if "scanning" in t.slot(L)]
        started = bool(scanning)
    t.need(scanning, "no slot is scanning")
    try:
        t.keys("MENU 0 7")
        time.sleep(1.0)
        for L in scanning:
            t.check("scanning" in t.slot(L), f"slot {L} still scanning under the menu", t.slot(L))
    finally:
        if started:
            t.home()
            t.keys("SIDE2")


@test("MENU-11", pages=("Home", "Value"))
def everyday_controls_off_the_menu(t):
    """Squelch, power, volume, scan and monitor are reachable from home keys: hold 3 squelch, hold 2 power,
    SIDE2 scan, SIDE1 monitor (volume is the knob on top)."""
    u = t.press("3", long=True)
    t.eq(u.id, "Value", "hold 3 opens the squelch editor")
    t.eq(u["value"]["args"][0], "Squelch", "the squelch")
    t.home()
    p0 = t.ui().active["power"]
    u = t.press("2", long=True)
    t.check(u.popup and u.popup["head"] == "TX power", "hold 2: the TX power popup", u.popup)
    t.check(u.active["power"] != p0, "the power changed", u.active)
    for _ in range(2):
        u = t.press("2", long=True)
    t.eq(t.ui(0.3).active["power"], p0, "three holds go round to where it was")
    u = t.keys("SIDE1")
    t.check(u.active.get("state") in ("Monitor", "MON", "RX") or "Monitor" in str(u.toast) or "Monitor" in u.text(),
            "SIDE1 tap: monitor", u.active)
    t.keys("SIDE1")
