#include "keyboard.h"

#include <chrono>
#include <cstring>
#include <sys/mman.h>
#include <unistd.h>

#include <QGuiApplication>
#include <wayland-client.h>

#include "virtual-keyboard.h"

namespace {

const char keymap[] = "xkb_keymap { xkb_keycodes { include \"evdev\" }; xkb_types { include \"complete\" }; "
                      "xkb_compat { include \"complete\" }; xkb_symbols { include \"pc+us\" }; };";

void global(void *data, wl_registry *registry, uint32_t name, const char *interface, uint32_t) {
    if (strcmp(interface, zwp_virtual_keyboard_manager_v1_interface.name) == 0)
        *static_cast<zwp_virtual_keyboard_manager_v1 **>(data) = static_cast<zwp_virtual_keyboard_manager_v1 *>(
            wl_registry_bind(registry, name, &zwp_virtual_keyboard_manager_v1_interface, 1));
}

void globalRemove(void *, wl_registry *, uint32_t) {}

const wl_registry_listener listener{global, globalRemove};

}

Keyboard::Keyboard(QObject *parent) : QObject(parent) {
    auto *wayland = qGuiApp->nativeInterface<QNativeInterface::QWaylandApplication>();
    if (!wayland || !wayland->seat()) {
        qWarning("Sliver.Keyboard: needs a Wayland seat");
        return;
    }
    display = wayland->display();
    queue = wl_display_create_queue(display);
    auto *wrapper = static_cast<wl_display *>(wl_proxy_create_wrapper(display));
    wl_proxy_set_queue(reinterpret_cast<wl_proxy *>(wrapper), queue);
    wl_registry *registry = wl_display_get_registry(wrapper);
    zwp_virtual_keyboard_manager_v1 *manager = nullptr;
    wl_registry_add_listener(registry, &listener, &manager);
    wl_display_roundtrip_queue(display, queue);
    wl_registry_destroy(registry);
    wl_proxy_wrapper_destroy(wrapper);
    if (!manager) {
        qWarning("Sliver.Keyboard: the compositor has no zwp_virtual_keyboard_manager_v1");
        return;
    }
    keyboard = zwp_virtual_keyboard_manager_v1_create_virtual_keyboard(manager, wayland->seat());
    zwp_virtual_keyboard_manager_v1_destroy(manager);
    int fd = memfd_create("keymap", MFD_CLOEXEC);
    if (fd >= 0 && write(fd, keymap, sizeof keymap) == sizeof keymap)
        zwp_virtual_keyboard_v1_keymap(keyboard, WL_KEYBOARD_KEYMAP_FORMAT_XKB_V1, fd, sizeof keymap);
    if (fd >= 0)
        close(fd);
    wl_display_flush(display);
}

Keyboard::~Keyboard() {
    if (keyboard) {
        zwp_virtual_keyboard_v1_destroy(keyboard);
        wl_display_flush(display);
    }
    if (queue)
        wl_event_queue_destroy(queue);
}

void Keyboard::key(int code, bool pressed) {
    if (!keyboard)
        return;
    auto ms = std::chrono::duration_cast<std::chrono::milliseconds>(std::chrono::steady_clock::now().time_since_epoch());
    zwp_virtual_keyboard_v1_key(keyboard, uint32_t(ms.count()), uint32_t(code), pressed);
    wl_display_flush(display);
}
