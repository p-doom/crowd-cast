#pragma once

#include <stdint.h>

int updater_init(void);
int updater_can_check_for_updates(void);
int updater_check_for_updates(void);
int updater_take_prepare_for_update_request(void);
void updater_set_busy(int busy);
const char *updater_last_error_message(void);
