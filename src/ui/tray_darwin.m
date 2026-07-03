/*
 * Tray icon library - macOS implementation
 * Based on dmikushin/tray (https://github.com/dmikushin/tray)
 * MIT License
 */

#import <Cocoa/Cocoa.h>
#include <stdatomic.h>
#include "tray.h"

static NSStatusItem *statusItem = nil;
static NSMenu *menu = nil;
static struct tray *currentTray = nil;
static BOOL shouldExit = NO;
static BOOL screenUnlocked = NO;
static CFAbsoluteTime trayInitTime = 0;
static id unlockObserver = nil;
static id wakeObserver = nil;
static atomic_bool restartPrepared = false;
static atomic_bool trayRestartRequested = false;
static CFAbsoluteTime statusItemDetachedSince = 0;
static BOOL statusItemWasAttached = NO;

// Login-session lock state, used to decide whether a wake should restart immediately. Declared
// here because it has no public header. Returns a dict with "CGSSessionScreenIsLocked" set when
// the screen is locked.
extern CFDictionaryRef CGSessionCopyCurrentDictionary(void);

// YES if the screen is locked. Conservatively returns YES (treat as locked) when the state can't
// be determined, so a wake-time restart is only ever issued when we are SURE the screen is
// already available — never while it may still be locked.
static BOOL screen_is_locked(void) {
    CFDictionaryRef session = CGSessionCopyCurrentDictionary();
    if (session == NULL) {
        return YES;
    }
    BOOL locked = YES;
    CFBooleanRef value = (CFBooleanRef)CFDictionaryGetValue(session, CFSTR("CGSSessionScreenIsLocked"));
    if (value == NULL) {
        // Key absent means "not locked" on a normal console session.
        locked = NO;
    } else {
        locked = CFBooleanGetValue(value) ? YES : NO;
    }
    CFRelease(session);
    return locked;
}

@interface TrayDelegate : NSObject <NSMenuDelegate, NSApplicationDelegate>
@end

@implementation TrayDelegate

- (void)menuItemClicked:(id)sender {
    NSMenuItem *item = (NSMenuItem *)sender;
    NSValue *value = item.representedObject;
    if (value != nil) {
        struct tray_menu *m = (struct tray_menu *)[value pointerValue];
        if (m && m->cb) {
            m->cb(m);
        }
    }
}

- (NSApplicationTerminateReply)applicationShouldTerminate:(NSApplication *)sender {
    // Called when macOS or Sparkle requests termination ([NSApp terminate:]).
    // Check if this exit was intentional (Sparkle update, already preparing).
    // If not (macOS sleep/hibernate/logout), exit with non-zero so
    // KeepAlive/Crashed restarts the process.
    extern bool is_intentional_exit(void);
    if (is_intentional_exit()) {
        return NSTerminateNow;  // exit(0), KeepAlive won't restart
    }
    _exit(1);  // Non-zero exit, KeepAlive/Crashed will restart
    return NSTerminateCancel;  // Never reached
}

@end

static TrayDelegate *delegate = nil;

static NSMenu *_tray_menu(struct tray_menu *m) {
    NSMenu *menu = [[NSMenu alloc] init];
    [menu setAutoenablesItems:NO];
    
    for (; m != NULL && m->text != NULL; m++) {
        if (strcmp(m->text, "-") == 0) {
            [menu addItem:[NSMenuItem separatorItem]];
        } else {
            NSMenuItem *item = [[NSMenuItem alloc]
                initWithTitle:[NSString stringWithUTF8String:m->text]
                action:@selector(menuItemClicked:)
                keyEquivalent:@""];
            
            [item setTarget:delegate];
            // Wrap the pointer in NSValue to safely store it as representedObject
            NSValue *ptrValue = [NSValue valueWithPointer:m];
            [item setRepresentedObject:ptrValue];
            [item setEnabled:m->disabled ? NO : YES];
            [item setState:m->checked ? NSControlStateValueOn : NSControlStateValueOff];
            
            if (m->submenu != NULL) {
                [item setSubmenu:_tray_menu(m->submenu)];
            }
            
            [menu addItem:item];
        }
    }
    
    return menu;
}

static NSStatusItem *create_status_item(void) {
    NSStatusItem *item = [[NSStatusBar systemStatusBar]
        statusItemWithLength:NSVariableStatusItemLength];
    if (item != nil) {
        statusItemDetachedSince = 0;
        statusItemWasAttached = NO;
    }
    return item;
}

static void request_tray_restart(NSString *reason) {
    bool alreadyRequested = atomic_exchange(&trayRestartRequested, true);
    if (!alreadyRequested) {
        NSLog(@"%@ - requesting process restart", reason);
    }
}

static void check_status_item_health(void) {
    if (atomic_load(&restartPrepared) || atomic_load(&trayRestartRequested)) {
        return;
    }

    // Throttle: this runs from the tray loop on every iteration (~60Hz) and
    // again from tray_update. Running the actual health logic that often burns
    // main-thread CPU for no benefit, so gate it to at most once per ~2s.
    static CFAbsoluteTime lastRun = 0;
    CFAbsoluteTime now = CFAbsoluteTimeGetCurrent();
    if (lastRun != 0 && now - lastRun < 2.0) {
        return;
    }
    lastRun = now;

    if (statusItem == nil || statusItem.button == nil) {
        request_tray_restart(@"Status item lost");
        return;
    }

    NSWindow *window = statusItem.button.window;
    if (window != nil) {
        statusItemWasAttached = YES;
        statusItemDetachedSince = 0;
        return;
    }

    if (statusItemDetachedSince == 0) {
        statusItemDetachedSince = now;
    }

    // A freshly-created status item may not have an attached window for a
    // moment while ControlCenter builds the backing scene. On newer macOS,
    // ControlCenter can transiently detach the window while it rebuilds the
    // status bar, then reattach within a few seconds — restarting on such a
    // blip caused a launch→exec restart storm. Only restart if an item that
    // was previously attached stays detached across many consecutive samples
    // (>15s, i.e. genuinely and persistently gone). In-process recreation has
    // proven unreliable on Tahoe, so ask Rust to restart with a fresh process
    // identity.
    if (statusItemWasAttached && now - statusItemDetachedSince > 15.0) {
        request_tray_restart(@"Status item is not attached to the menu bar");
    }
}

int tray_init(struct tray *tray) {
    @autoreleasepool {
        NSApplication *app = [NSApplication sharedApplication];
        [app setActivationPolicy:NSApplicationActivationPolicyAccessory];

        delegate = [[TrayDelegate alloc] init];
        [app setDelegate:delegate];

        // Briefly start [NSApp run] and immediately stop it.
        // [NSApp run] registers the Apple Event Mach port on the main run
        // loop and installs default AE handlers (including kAEQuitApplication).
        // Our custom event loop bypasses [NSApp run], so without this the
        // Sparkle Updater's [NSRunningApplication terminate] Apple Event goes
        // unhandled (-1708) and the app never quits for updates.
        dispatch_async(dispatch_get_main_queue(), ^{
            [NSApp stop:nil];
            [NSApp postEvent:[NSEvent otherEventWithType:NSEventTypeApplicationDefined
                                                location:NSZeroPoint
                                           modifierFlags:0
                                               timestamp:0
                                            windowNumber:0
                                                 context:nil
                                                 subtype:0
                                                   data1:0
                                                   data2:0]
                     atStart:YES];
        });
        [NSApp run];

        // Register for screen-unlock notification to trigger restart.
        // After sleep→wake→unlock, SCK capture sources are stale and need
        // a full process restart to reinitialise.
        // Grace period: ignore unlocks within 30s of launch to avoid a
        // restart loop (the freshly restarted process would see the same
        // unlock notification and exit again).
        trayInitTime = CFAbsoluteTimeGetCurrent();
        unlockObserver = [[NSDistributedNotificationCenter defaultCenter]
            addObserverForName:@"com.apple.screenIsUnlocked"
                        object:nil
                         queue:[NSOperationQueue mainQueue]
                    usingBlock:^(NSNotification *note) {
                        CFAbsoluteTime elapsed = CFAbsoluteTimeGetCurrent() - trayInitTime;
                        if (elapsed < 30.0) {
                            NSLog(@"Screen unlocked — ignoring (%.1fs since launch, grace period)", elapsed);
                            return;
                        }
                        NSLog(@"Screen unlocked — scheduling restart for fresh capture sources");
                        screenUnlocked = YES;
                    }];

        // Also restart on wake itself, not just unlock. `com.apple.screenIsUnlocked` only fires
        // if the screen actually locked on sleep — with "require password after sleep" off, a
        // sleep→wake produces no unlock event, so a recording would straddle the suspend and drift.
        // NSWorkspaceDidWakeNotification fires on every resume regardless of lock state; it feeds
        // the SAME restart flag, and the 30s launch grace + the flag dedupe against a following
        // unlock so we never double-restart.
        wakeObserver = [[[NSWorkspace sharedWorkspace] notificationCenter]
            addObserverForName:NSWorkspaceDidWakeNotification
                        object:nil
                         queue:[NSOperationQueue mainQueue]
                    usingBlock:^(NSNotification *note) {
                        CFAbsoluteTime elapsed = CFAbsoluteTimeGetCurrent() - trayInitTime;
                        if (elapsed < 30.0) {
                            NSLog(@"Woke from sleep — ignoring (%.1fs since launch, grace period)", elapsed);
                            return;
                        }
                        // If the screen locked on sleep (the common case), DON'T restart here:
                        // the `com.apple.screenIsUnlocked` observer fires on unlock and restarts
                        // then, when the screen is available for ScreenCaptureKit to re-init.
                        // Restarting while locked could leave capture unable to re-acquire. Only
                        // act on the no-lock case (e.g. "require password after sleep" off), where
                        // no unlock event will ever come and the screen is already available.
                        if (screen_is_locked()) {
                            NSLog(@"Woke from sleep — screen locked, deferring to unlock observer");
                            return;
                        }
                        NSLog(@"Woke from sleep (screen not locked) — scheduling restart for fresh capture sources");
                        screenUnlocked = YES;
                    }];

        // Must be called on main thread - dispatch if needed
        if ([NSThread isMainThread]) {
            statusItem = create_status_item();
        } else {
            dispatch_sync(dispatch_get_main_queue(), ^{
                statusItem = create_status_item();
            });
        }
        
        if (statusItem == nil) {
            return -1;
        }
        
        currentTray = tray;
        shouldExit = NO;
        
        tray_update(tray);
        
        return 0;
    }
}

int tray_loop(int blocking) {
    @autoreleasepool {
        @try {
            if (shouldExit) {
                return -1;
            }
            
            // Process pending events without blocking
            NSApplication *app = [NSApplication sharedApplication];
            NSEvent *event;
            while ((event = [app nextEventMatchingMask:NSEventMaskAny
                                            untilDate:blocking ? [NSDate distantFuture] : [NSDate distantPast]
                                               inMode:NSDefaultRunLoopMode
                                              dequeue:YES])) {
                [app sendEvent:event];

                // Check if we should exit after each event
                if (shouldExit) {
                    return -1;
                }
            }

            // Drain the main GCD queue and run loop sources.
            // Sparkle's XPC installer dispatches [NSApp terminate:] to the
            // main queue — nextEventMatchingMask: alone won't process it.
            [[NSRunLoop currentRunLoop] runMode:NSDefaultRunLoopMode
                                     beforeDate:[NSDate distantPast]];
            check_status_item_health();
            
            return shouldExit ? -1 : 0;
        } @catch (NSException *exception) {
            // Log exception but don't crash
            NSLog(@"Tray loop exception: %@", exception);
            return 0;
        }
    }
}

void tray_update(struct tray *tray) {
    void (^updateBlock)(void) = ^{
        @autoreleasepool {
            @try {
                if (atomic_load(&restartPrepared)) {
                    return;
                }

                currentTray = tray;

                // Recover from macOS dropping the status item. On Tahoe, trying
                // to recreate it repeatedly in-process can wedge ControlCenter;
                // ask the Rust side for a fresh process instead.
                if (statusItem == nil || statusItem.button == nil) {
                    request_tray_restart(@"Status item lost during update");
                    return;
                }

                // Always ensure visible (macOS can hide items after display changes)
                statusItem.visible = YES;

                // Update icon
                if (tray->icon_filepath != NULL) {
                    NSString *path = [NSString stringWithUTF8String:tray->icon_filepath];
                    NSImage *image = [[NSImage alloc] initWithContentsOfFile:path];
                    if (image != nil) {
                        [image setSize:NSMakeSize(18, 18)];
                        [image setTemplate:NO];
                        statusItem.button.image = image;
                    }
                }

                // Update tooltip
                if (tray->tooltip != NULL) {
                    statusItem.button.toolTip = [NSString stringWithUTF8String:tray->tooltip];
                }

                // Update menu
                if (tray->menu != NULL) {
                    menu = _tray_menu(tray->menu);
                    statusItem.menu = menu;
                }
                check_status_item_health();
            } @catch (NSException *exception) {
                NSLog(@"Tray update exception: %@", exception);
            }
        }
    };
    
    // UI updates must happen on main thread
    if ([NSThread isMainThread]) {
        updateBlock();
    } else {
        dispatch_async(dispatch_get_main_queue(), updateBlock);
    }
}

static void tray_teardown_on_main(void) {
    if (unlockObserver != nil) {
        [[NSDistributedNotificationCenter defaultCenter] removeObserver:unlockObserver];
        unlockObserver = nil;
    }

    if (wakeObserver != nil) {
        [[[NSWorkspace sharedWorkspace] notificationCenter] removeObserver:wakeObserver];
        wakeObserver = nil;
    }

    if (statusItem != nil) {
        [[NSStatusBar systemStatusBar] removeStatusItem:statusItem];
        statusItem = nil;
    }

    menu = nil;
    currentTray = nil;
    statusItemDetachedSince = 0;
    statusItemWasAttached = NO;

    // Tahoe's ControlCenter/status-item host can reject a newly-created
    // NSStatusItem if the old item has not fully disconnected yet. Let the
    // removal notification and scene invalidation drain before replacing the
    // process.
    NSDate *deadline = [NSDate dateWithTimeIntervalSinceNow:0.25];
    while ([deadline timeIntervalSinceNow] > 0) {
        @autoreleasepool {
            [[NSRunLoop currentRunLoop] runMode:NSDefaultRunLoopMode
                                     beforeDate:deadline];
        }
    }
}

void tray_prepare_for_restart(void) {
    bool alreadyPrepared = atomic_exchange(&restartPrepared, true);
    if (alreadyPrepared) {
        return;
    }

    if ([NSThread isMainThread]) {
        tray_teardown_on_main();
    } else {
        dispatch_semaphore_t done = dispatch_semaphore_create(0);
        dispatch_async(dispatch_get_main_queue(), ^{
            tray_teardown_on_main();
            dispatch_semaphore_signal(done);
        });

        intptr_t result = dispatch_semaphore_wait(
            done,
            dispatch_time(DISPATCH_TIME_NOW, (int64_t)(1 * NSEC_PER_SEC))
        );
        if (result != 0) {
            NSLog(@"Timed out waiting for tray teardown before restart");
        }
    }
}

void tray_exit(void) {
    shouldExit = YES;
}

bool tray_screen_was_unlocked(void) {
    if (screenUnlocked) {
        screenUnlocked = NO;
        return true;
    }
    return false;
}

bool tray_needs_restart(void) {
    if (atomic_exchange(&trayRestartRequested, false)) {
        return true;
    }
    return false;
}
