// Starts a system drag of files from a window, and reports where the
// pointer is while another app's drag is over it. Called from platform.rs.

#import <AppKit/AppKit.h>

@interface ArmatureDragSource : NSObject <NSDraggingSource>
@end

@implementation ArmatureDragSource
- (NSDragOperation)draggingSession:(NSDraggingSession *)session sourceOperationMaskForDraggingContext:(NSDraggingContext)context {
    // The app the files land in decides whether to copy or move them.
    return NSDragOperationCopy | NSDragOperationMove | NSDragOperationLink | NSDragOperationGeneric;
}
@end

// Begins dragging the files at `paths` from `ns_view`. Must be called while
// the mouse event that started the drag is being handled. Returns 1 if a
// drag began.
int armature_drag_files(void *ns_view, const char *const *paths, int count) {
    @autoreleasepool {
        NSView *view = (__bridge NSView *)ns_view;
        NSEvent *event = NSApp.currentEvent;
        if (view == nil || event == nil || count <= 0) {
            return 0;
        }
        if (event.type != NSEventTypeLeftMouseDown && event.type != NSEventTypeLeftMouseDragged) {
            return 0;
        }
        NSPoint at = [view convertPoint:event.locationInWindow fromView:nil];
        NSMutableArray<NSDraggingItem *> *items = [NSMutableArray array];
        for (int i = 0; i < count; i++) {
            NSString *path = @(paths[i]);
            NSDraggingItem *item = [[NSDraggingItem alloc] initWithPasteboardWriter:[NSURL fileURLWithPath:path]];
            NSImage *icon = [[NSWorkspace sharedWorkspace] iconForFile:path];
            // A small fan of icons, the first under the pointer.
            CGFloat shift = MIN(i, 4) * 6.0;
            [item setDraggingFrame:NSMakeRect(at.x - 20 + shift, at.y - 20 - shift, 40, 40) contents:icon];
            [items addObject:item];
        }
        static ArmatureDragSource *source;
        if (source == nil) {
            source = [ArmatureDragSource new];
        }
        NSDraggingSession *session = [view beginDraggingSessionWithItems:items event:event source:source];
        session.animatesToStartingPositionsOnCancelOrFail = YES;
        return 1;
    }
}

// The pointer's position in the view, from the top-left corner in points.
// Works during a drag from another app, when no mouse events arrive.
int armature_pointer_in_view(void *ns_view, double *x, double *y) {
    NSView *view = (__bridge NSView *)ns_view;
    if (view == nil || view.window == nil) {
        return 0;
    }
    NSPoint p = [view convertPoint:view.window.mouseLocationOutsideOfEventStream fromView:nil];
    *x = p.x;
    *y = view.isFlipped ? p.y : view.bounds.size.height - p.y;
    return 1;
}
