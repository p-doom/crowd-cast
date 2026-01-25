/*
 * Windows platform implementation for frontmost application detection.
 * Uses GetForegroundWindow to get the focused window, then extracts
 * the executable name of the owning process.
 */

#ifdef _WIN32

#include <windows.h>
#include <psapi.h>
#include <stdlib.h>
#include <string.h>
#include <ctype.h>
#include "platform.h"

char *platform_get_frontmost_app_id(void)
{
    HWND hwnd = GetForegroundWindow();
    if (!hwnd) {
        return NULL;
    }
    
    DWORD pid = 0;
    GetWindowThreadProcessId(hwnd, &pid);
    if (pid == 0) {
        return NULL;
    }
    
    HANDLE process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, FALSE, pid);
    if (!process) {
        return NULL;
    }
    
    char exe_path[MAX_PATH];
    DWORD path_size = MAX_PATH;
    
    if (!QueryFullProcessImageNameA(process, 0, exe_path, &path_size)) {
        CloseHandle(process);
        return NULL;
    }
    CloseHandle(process);
    
    /* Extract just the executable name from the full path */
    char *exe_name = strrchr(exe_path, '\\');
    if (exe_name) {
        exe_name++; /* Skip the backslash */
    } else {
        exe_name = exe_path;
    }
    
    return strdup(exe_name);
}

bool platform_is_wayland(void)
{
    /* Windows doesn't have Wayland */
    return false;
}

/* Helper: case-insensitive string comparison */
static int strcasecmp_win(const char *s1, const char *s2)
{
    while (*s1 && *s2) {
        int c1 = tolower((unsigned char)*s1);
        int c2 = tolower((unsigned char)*s2);
        if (c1 != c2) {
            return c1 - c2;
        }
        s1++;
        s2++;
    }
    return tolower((unsigned char)*s1) - tolower((unsigned char)*s2);
}

bool platform_app_ids_match(const char *frontmost_id, const char *target_id)
{
    if (!frontmost_id || !target_id) {
        return false;
    }
    
    /*
     * On Windows, the target_id from window_capture source settings varies:
     * - Could be window title
     * - Could be window class
     * - Could be executable name
     * 
     * The frontmost_id we return is the executable name (e.g., "Code.exe").
     * We do case-insensitive comparison and also check for substring matches.
     */
    
    /* Direct case-insensitive match */
    if (strcasecmp_win(frontmost_id, target_id) == 0) {
        return true;
    }
    
    /* Check if target contains our exe name (without .exe) */
    size_t front_len = strlen(frontmost_id);
    if (front_len > 4 && strcasecmp_win(frontmost_id + front_len - 4, ".exe") == 0) {
        /* Create version without .exe */
        char *without_ext = strdup(frontmost_id);
        if (without_ext) {
            without_ext[front_len - 4] = '\0';
            
            /* Check if target contains the exe name without extension */
            char *target_lower = strdup(target_id);
            char *front_lower = strdup(without_ext);
            if (target_lower && front_lower) {
                for (char *p = target_lower; *p; p++) *p = tolower((unsigned char)*p);
                for (char *p = front_lower; *p; p++) *p = tolower((unsigned char)*p);
                
                bool found = strstr(target_lower, front_lower) != NULL;
                free(target_lower);
                free(front_lower);
                free(without_ext);
                if (found) return true;
            } else {
                free(target_lower);
                free(front_lower);
                free(without_ext);
            }
        }
    }
    
    return false;
}

#endif /* _WIN32 */
