# gui_interact.tcl - per-interaction testing: inject real events with `event generate`, assert STATE change.
#
# Every leg drives a real widget through a real event and predicts the outcome:
#   - button + command -> Enter + ButtonPress-1 + ButtonRelease-1 fires the command exactly once, twice on a
#     second click (real event routing through Tk's binding tables, not a direct proc call); a *disabled*
#     button's `invoke` does NOT fire (negative control).
#   - checkbutton -> `invoke` toggles the linked variable to 1; a second toggles back to 0; a Space keypress
#     on a focused checkbutton also toggles it (real key binding).
#   - entry -> keysym KeyPress events land in the text; backspace removes; delete/insert index ops are exact.
#   - scale -> `set` then a keyboard step (Right/Up) moves value() by the exact resolution; from/to clamp.
#   - listbox -> insert/size/get/curselection/delete are exact; a selection-set changes curselection.
#
# Xvfb provides the headless X display; events are posted through the real Tk event loop against mapped
# widgets. `update` flushes the queue so the state settles before we read it.

package require Tk
wm geometry . 500x500+0+0
source [file join [file dirname [info script]] gui_common.tcl]
gate_init GUI_INTERACT

# ------------------------------------------------------------------ button: real click fires the command
proc leg_button_click {} {
    set ::fired 0
    button .b -text Go -command {incr ::fired}
    pack .b
    update idletasks; update
    set w [winfo width .b]; set h [winfo height .b]
    # a real synthesized click: pointer enters, presses and releases inside the button
    event generate .b <Enter>
    event generate .b <ButtonPress-1>   -x [expr {$w/2}] -y [expr {$h/2}]
    event generate .b <ButtonRelease-1> -x [expr {$w/2}] -y [expr {$h/2}]
    update
    check {$::fired == 1} "button: first synthesized click did not fire command exactly once"
    event generate .b <ButtonPress-1>   -x [expr {$w/2}] -y [expr {$h/2}]
    event generate .b <ButtonRelease-1> -x [expr {$w/2}] -y [expr {$h/2}]
    update
    check {$::fired == 2} "button: second click did not increment to two"
    destroy .b
}

# ------------------------------------------------------------------ button disabled: negative control
proc leg_button_disabled {} {
    set ::fired2 0
    button .bd -text Off -state disabled -command {incr ::fired2}
    pack .bd
    update
    .bd invoke   ;# invoke on a disabled button must be a no-op
    update
    check {$::fired2 == 0} "button(disabled): invoke wrongly fired the command"
    destroy .bd
}

# ------------------------------------------------------------------ checkbutton: variable + key toggle
proc leg_checkbutton {} {
    set ::cbv 0
    checkbutton .cb -variable ::cbv -text opt
    pack .cb
    update
    check {$::cbv == 0} "checkbutton: initial variable not 0"
    .cb invoke
    check {$::cbv == 1} "checkbutton: invoke did not set variable to 1"
    .cb invoke
    check {$::cbv == 0} "checkbutton: second invoke did not toggle back to 0"
    # a Space key on the focused checkbutton toggles it (real key binding)
    focus -force .cb
    update
    event generate .cb <KeyPress-space>
    event generate .cb <KeyRelease-space>
    update
    check {$::cbv == 1} "checkbutton: Space key did not toggle to 1"
    destroy .cb
}

# ------------------------------------------------------------------ entry: keystrokes + edit ops
proc leg_entry {} {
    entry .e
    pack .e
    update
    focus -force .e
    update
    foreach k {h e l l o} { event generate .e <KeyPress> -keysym $k }
    update
    check {[.e get] eq "hello"} "entry: keysym KeyPress events did not produce 'hello'"
    event generate .e <KeyPress> -keysym BackSpace
    update
    check {[.e get] eq "hell"} "entry: BackSpace did not yield 'hell'"
    # exact index ops
    .e delete 0 end
    .e insert 0 "world"
    check {[.e get] eq "world"}       "entry: insert did not set 'world'"
    check {[.e index end] == 5}       "entry: end index != 5"
    .e delete 0 1
    check {[.e get] eq "orld"}        "entry: delete 0 1 did not drop first char"
    .e icursor 2
    .e insert insert "XY"
    check {[.e get] eq "orXYld"}      "entry: insert at cursor 2 wrong"
    destroy .e
}

# ------------------------------------------------------------------ scale: set + keyboard step + clamp
proc leg_scale {} {
    scale .s -from 0 -to 100 -orient horizontal -resolution 1
    pack .s
    update
    .s set 42
    check {[.s get] == 42} "scale: set 42 not read back"
    focus -force .s
    update
    # for a horizontal scale, Right moves +resolution and Left -resolution (1)
    event generate .s <KeyPress-Right>
    update
    check {[.s get] == 43} "scale: Right did not add one step (43)"
    event generate .s <KeyPress-Left>
    update
    check {[.s get] == 42} "scale: Left did not subtract one step (42)"
    # clamp to bounds
    .s set 200
    check {[.s get] == 100} "scale: set above max not clamped to 100"
    .s set -50
    check {[.s get] == 0}   "scale: set below min not clamped to 0"
    destroy .s
}

# ------------------------------------------------------------------ listbox: insert/get/selection/delete
proc leg_listbox {} {
    listbox .lb
    pack .lb
    update
    .lb insert end alpha beta gamma delta
    check {[.lb size] == 4}                 "listbox: size != 4 after insert"
    check {[.lb get 1] eq "beta"}           "listbox: get 1 != beta"
    check {[.lb get 0 end] eq {alpha beta gamma delta}} "listbox: get range wrong"
    .lb selection set 2
    check {[.lb curselection] eq "2"}       "listbox: curselection != 2 after selection set"
    check {[.lb selection includes 2]}      "listbox: selection includes 2 false"
    .lb delete 0
    check {[.lb size] == 3}                 "listbox: size != 3 after delete 0"
    check {[.lb get 0] eq "beta"}           "listbox: get 0 != beta after delete"
    destroy .lb
}

leg_button_click
leg_button_disabled
leg_checkbutton
leg_entry
leg_scale
leg_listbox
gate_finish
