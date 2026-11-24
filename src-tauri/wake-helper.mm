// Installed as a root-owned, one-shot launchd job. Reads one user's bounded
// plan; its only privileged capability is scheduling/cancelling its own wake.
#import <Foundation/Foundation.h>
#import <IOKit/pwr_mgt/IOPMLib.h>
#include <sys/stat.h>
#include <fcntl.h>
#include <unistd.h>
#include <libproc.h>
#include <cmath>

static NSDictionary *readPlan(NSString *file, uid_t owner) {
  int fd = open(file.fileSystemRepresentation, O_RDONLY | O_NOFOLLOW | O_NONBLOCK);
  if (fd < 0) return nil;
  struct stat st;
  if (fstat(fd, &st) || !S_ISREG(st.st_mode) || st.st_uid != owner || (st.st_mode & 0022) || st.st_size < 2 || st.st_size > 4096) { close(fd); return nil; }
  char data[4097]; ssize_t n = read(fd, data, sizeof(data)); close(fd);
  if (n < 2 || n > 4096) return nil;
  id value = [NSJSONSerialization JSONObjectWithData:[NSData dataWithBytes:data length:n] options:0 error:nullptr];
  return [value isKindOfClass:NSDictionary.class] ? value : nil;
}
static double number(id value) { return [value isKindOfClass:NSNumber.class] ? [value doubleValue] : 0; }
static bool appRunning(NSDictionary *plan, uid_t owner) {
  double raw = number(plan[@"pid"]);
  if (!std::isfinite(raw) || raw < 1 || raw > INT_MAX || floor(raw) != raw) return false;
  struct proc_bsdinfo info = {};
  if (proc_pidinfo((int)raw, PROC_PIDTBSDINFO, 0, &info, sizeof(info)) != sizeof(info) || info.pbi_uid != owner) return false;
  char executable[PROC_PIDPATHINFO_MAXSIZE] = {};
  if (proc_pidpath((int)raw, executable, sizeof(executable)) <= 0) return false;
  return [[NSString stringWithUTF8String:executable] hasSuffix:@"/MeterNest.app/Contents/MacOS/MeterNest"];
}
int main(int argc, const char *argv[]) { @autoreleasepool {
  if (geteuid() != 0 || argc != 3) return 77;
  char *end = nullptr; long user = strtol(argv[1], &end, 10);
  if (!end || *end || user < 501 || user > INT_MAX) return 64;
  uid_t uid = (uid_t)user;
  NSString *home = [NSString stringWithUTF8String:argv[2]];
  struct stat homeStat;
  if (![home hasPrefix:@"/Users/"] || lstat(home.fileSystemRepresentation, &homeStat) || !S_ISDIR(homeStat.st_mode) || homeStat.st_uid != uid) return 64;
  NSString *owner = [NSString stringWithFormat:@"com.meternest.desktop.wake.%u", uid];
  NSString *statusPath = [NSString stringWithFormat:@"/var/db/com.meternest.wake.%u.json", uid];
  NSString *input = [home stringByAppendingPathComponent:@"Library/Application Support/MeterNest/wake-plan.json"];
  NSDictionary *plan = readPlan(input, uid), *previous = readPlan(statusPath, 0);
  double now = NSDate.date.timeIntervalSince1970 * 1000, target = number(plan[@"wakeAt"]);
  bool enabled = [plan[@"enabled"] isEqual:@YES] && appRunning(plan, uid);
  if (!enabled || !std::isfinite(target) || target <= now + 1000 || target > now + 8 * 86400000.0) target = 0;
  double old = number(previous[@"wakeAt"]);
  if (old == target && ((target > 0 && [previous[@"outcome"] isEqual:@"scheduled"]) || (target == 0 && [previous[@"outcome"] isEqual:@"cancelled"]))) return 0;
  if (old > 0) IOPMCancelScheduledPowerEvent((__bridge CFDateRef)[NSDate dateWithTimeIntervalSince1970:old / 1000], (__bridge CFStringRef)owner, CFSTR(kIOPMAutoWake));
  IOReturn result = kIOReturnSuccess;
  if (target > 0) result = IOPMSchedulePowerEvent((__bridge CFDateRef)[NSDate dateWithTimeIntervalSince1970:target / 1000], (__bridge CFStringRef)owner, CFSTR(kIOPMAutoWake));
  NSDictionary *status = @{@"wakeAt": @(result == kIOReturnSuccess ? target : 0), @"requestedWakeAt": @(target), @"outcome": result != kIOReturnSuccess ? @"error" : target > 0 ? @"scheduled" : @"cancelled", @"at": @(now)};
  NSData *json = [NSJSONSerialization dataWithJSONObject:status options:0 error:nullptr];
  if (![json writeToFile:statusPath options:NSDataWritingAtomic error:nullptr]) return 74;
  chmod(statusPath.fileSystemRepresentation, 0644);
  return result == kIOReturnSuccess ? 0 : 1;
} }
