
# GUI Support (X11)

StarryOS supports X11 for its graphical user interface.

## Steps

1. Make sure you run StarryOS with necessary flags:

```bash
make run ARCH=riscv64 GRAPHIC=y INPUT=y
```

2. In StarryOS, install necessary dependencies:

```bash
apk add xorg-server xf86-video-fbdev xf86-input-evdev
```

3. Edit `/etc/X11/xorg.conf` to configure the X server:

```
Section "Device"
    Identifier "MyFramebuffer"
    Driver "fbdev"
    Option "SWCursor" "on"
    Option "fbdev" "/dev/fb0"
EndSection

Section "Screen"
    Identifier "Default Screen"
    Device "MyFramebuffer"
    Monitor "Generic Monitor"
EndSection

Section "Monitor"
    Identifier "Generic Monitor"
EndSection

Section "ServerLayout"
    Identifier "Default Layout"
    Screen 0 "Default Screen"
    Option "AutoAddDevices" "false"
    InputDevice "Keyboard0" "CoreKeyboard"
    InputDevice "Mouse0" "CorePointer"
EndSection

Section "InputDevice"
    Identifier "Keyboard0"
    Driver "evdev"
    Option "Device" "/dev/input/event0"
EndSection

Section "InputDevice"
    Identifier "Mouse0"
    Driver "evdev"
    Option "Device" "/dev/input/mice"
EndSection
```

4. Start the X server and set the DISPLAY environment variable:

```bash
X &
export DISPLAY=:0
```

5. You can now run graphical applications, for example:

```bash
apk add xcalc
xcalc
```

`dwm` is also available as a lightweight window manager.

```bash
apk add dwm
dwm &
```
