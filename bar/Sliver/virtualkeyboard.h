#pragma once

#include <QObject>
#include <QtQml/qqmlregistration.h>

struct wl_display;
struct wl_event_queue;
struct zwp_virtual_keyboard_v1;

// A keyboard of its own on the desktop: key(code, pressed) with evdev keycodes.
class VirtualKeyboard : public QObject {
    Q_OBJECT
    QML_ELEMENT

public:
    explicit VirtualKeyboard(QObject *parent = nullptr);
    ~VirtualKeyboard() override;

    Q_INVOKABLE void key(int code, bool pressed);

private:
    wl_display *display = nullptr;
    wl_event_queue *queue = nullptr;
    zwp_virtual_keyboard_v1 *keyboard = nullptr;
};
