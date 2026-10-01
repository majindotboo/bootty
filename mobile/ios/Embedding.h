#include <stdbool.h>
void bootty_mobile_register(void);
void bootty_mobile_reload(void);
void bootty_mobile_disconnect(void);
void bootty_mobile_active(bool active);
void bootty_mobile_appearance(bool dark, float text_size);
void gpui_ios_run_demo(void);
void gpui_ios_set_embedded(void);
void *gpui_ios_get_window(void);
void *gpui_ios_view_controller(void *window);
void gpui_ios_layout_view(void *window);
bool gpui_ios_request_frame(void *window);
void gpui_ios_set_frame_waker(void *window, void (*waker)(void *context), void *context);
void gpui_ios_did_become_active(void *app);
void gpui_ios_will_resign_active(void *app);
