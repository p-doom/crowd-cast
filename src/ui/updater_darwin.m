#import "updater_darwin.h"

#import <Cocoa/Cocoa.h>
#import <Sparkle/Sparkle.h>

#include <stdatomic.h>
#include <string.h>

static char g_last_error_message[1024] = {0};
static atomic_bool g_prepare_update_requested = false;

static void crowdcast_set_last_error(NSString *message) {
    g_last_error_message[0] = '\0';
    if (message == nil || message.length == 0) {
        return;
    }

    const char *utf8 = message.UTF8String;
    if (utf8 == NULL) {
        return;
    }

    strlcpy(g_last_error_message, utf8, sizeof(g_last_error_message));
}

@interface CrowdCastUpdaterDelegate : NSObject <SPUUpdaterDelegate>
@property (atomic, assign) BOOL busy;
@property (atomic, copy, nullable) void (^pendingInstallHandler)(void);
@end

@interface CrowdCastUserDriverDelegate : NSObject <SPUStandardUserDriverDelegate>
@end

@implementation CrowdCastUpdaterDelegate

- (BOOL)updater:(SPUUpdater *)updater
mayPerformUpdateCheck:(SPUUpdateCheck)updateCheck
          error:(NSError * _Nullable __autoreleasing *)error {
    // Always allow checks — blocking here breaks Sparkle's scheduled timer.
    // Install postponement is handled by shouldPostponeRelaunchForUpdate: instead.
    return YES;
}

- (BOOL)updater:(SPUUpdater *)updater
shouldPostponeRelaunchForUpdate:(SUAppcastItem *)item
untilInvokingBlock:(void (^)(void))installHandler {
    if (!self.busy) {
        return NO;
    }

    // Notify the user that recording is being stopped for the update.
    extern void notifications_show_update_installing(void);
    notifications_show_update_installing();

    self.pendingInstallHandler = [installHandler copy];
    atomic_store(&g_prepare_update_requested, true);
    return YES;
}

- (BOOL)updater:(SPUUpdater *)updater
       willInstallUpdateOnQuit:(SUAppcastItem *)item
    immediateInstallationBlock:(void (^)(void))immediateInstallHandler {
    NSLog(@"[CrowdCast] willInstallUpdateOnQuit: busy=%d, version=%@",
          self.busy, item.versionString);

    if (!self.busy) {
        NSLog(@"[CrowdCast] Not busy — invoking immediate silent install");
        immediateInstallHandler();
        return YES;
    }

    NSLog(@"[CrowdCast] Busy — deferring install until recording stops");
    extern void notifications_show_update_installing(void);
    notifications_show_update_installing();

    self.pendingInstallHandler = [immediateInstallHandler copy];
    atomic_store(&g_prepare_update_requested, true);
    return YES;
}

- (void)updater:(SPUUpdater *)updater didFindValidUpdate:(SUAppcastItem *)item {
    NSLog(@"[CrowdCast] Sparkle found valid update: %@", item.versionString);
}

- (void)updater:(SPUUpdater *)updater didDownloadUpdate:(SUAppcastItem *)item {
    NSLog(@"[CrowdCast] Sparkle downloaded update: %@", item.versionString);
}

- (void)updater:(SPUUpdater *)updater willInstallUpdate:(SUAppcastItem *)item {
    NSLog(@"[CrowdCast] Sparkle will install update: %@", item.versionString);
}

- (void)updater:(SPUUpdater *)updater didAbortWithError:(NSError *)error {
    if (error == nil) {
        return;
    }

    NSLog(@"[CrowdCast] Sparkle aborted: %@", error.localizedDescription);
    crowdcast_set_last_error(error.localizedDescription);
}

@end

@implementation CrowdCastUserDriverDelegate

- (BOOL)supportsGentleScheduledUpdateReminders {
    return YES;
}

- (BOOL)standardUserDriverShouldHandleShowingScheduledUpdate:(SUAppcastItem *)update
                                          andInImmediateFocus:(BOOL)immediateFocus {
    // Let the standard driver handle the UI as a fallback.
    // Silent auto-updates are handled by willInstallUpdateOnQuit:immediateInstallationBlock:
    // on the updater delegate, which fires before any UI is shown.
    return YES;
}

@end

static CrowdCastUpdaterDelegate *g_updater_delegate = nil;
static CrowdCastUserDriverDelegate *g_user_driver_delegate = nil;
static SPUStandardUpdaterController *g_updater_controller = nil;

static BOOL crowdcast_is_valid_string(id _Nullable value) {
    return [value isKindOfClass:[NSString class]] && [(NSString *)value length] > 0;
}

int updater_init(void) {
    if (g_updater_controller != nil) {
        return 0;
    }

    @autoreleasepool {
        NSBundle *mainBundle = [NSBundle mainBundle];
        NSString *feedURL = [mainBundle objectForInfoDictionaryKey:@"SUFeedURL"];
        NSString *publicKey = [mainBundle objectForInfoDictionaryKey:@"SUPublicEDKey"];

        if (!crowdcast_is_valid_string(feedURL)) {
            crowdcast_set_last_error(@"SUFeedURL is missing from the app bundle.");
            return -1;
        }

        if (!crowdcast_is_valid_string(publicKey)) {
            crowdcast_set_last_error(@"SUPublicEDKey is missing from the app bundle.");
            return -1;
        }

        g_updater_delegate = [[CrowdCastUpdaterDelegate alloc] init];
        g_user_driver_delegate = [[CrowdCastUserDriverDelegate alloc] init];
        g_updater_controller = [[SPUStandardUpdaterController alloc]
            initWithStartingUpdater:NO
                    updaterDelegate:g_updater_delegate
                 userDriverDelegate:g_user_driver_delegate];

        if (g_updater_controller == nil) {
            crowdcast_set_last_error(@"Failed to create the Sparkle updater controller.");
            return -1;
        }

        crowdcast_set_last_error(nil);
        [g_updater_controller startUpdater];
        return 0;
    }
}

int updater_can_check_for_updates(void) {
    if (g_updater_controller == nil) {
        return 0;
    }

    return g_updater_controller.updater.canCheckForUpdates ? 1 : 0;
}

int updater_check_for_updates(void) {
    if (g_updater_controller == nil) {
        crowdcast_set_last_error(@"Sparkle updater has not been initialized.");
        return -1;
    }

    if (!g_updater_controller.updater.canCheckForUpdates) {
        crowdcast_set_last_error(@"Sparkle cannot check for updates right now.");
        return -1;
    }

    crowdcast_set_last_error(nil);
    [g_updater_controller checkForUpdates:nil];
    return 0;
}

int updater_check_for_updates_in_background(void) {
    if (g_updater_controller == nil) {
        return -1;
    }

    if (!g_updater_controller.updater.canCheckForUpdates) {
        return -1;
    }

    [g_updater_controller.updater checkForUpdatesInBackground];
    return 0;
}

int updater_take_prepare_for_update_request(void) {
    return atomic_exchange(&g_prepare_update_requested, false) ? 1 : 0;
}

void updater_set_busy(int busy) {
    if (g_updater_delegate == nil) {
        return;
    }

    g_updater_delegate.busy = (busy != 0);

    if (g_updater_delegate.busy || g_updater_delegate.pendingInstallHandler == nil) {
        return;
    }

    void (^installHandler)(void) = [g_updater_delegate.pendingInstallHandler copy];
    g_updater_delegate.pendingInstallHandler = nil;
    installHandler();

    // Fallback self-termination: if Sparkle's Apple Event quit request
    // isn't processed by our event loop within 5 seconds, exit anyway.
    // The recording has already been stopped and uploaded by this point.
    extern void tray_exit(void);
    dispatch_after(dispatch_time(DISPATCH_TIME_NOW, (int64_t)(5 * NSEC_PER_SEC)),
                   dispatch_get_main_queue(), ^{
        tray_exit();
        _exit(0);
    });
}

const char *updater_last_error_message(void) {
    return g_last_error_message[0] == '\0' ? NULL : g_last_error_message;
}
