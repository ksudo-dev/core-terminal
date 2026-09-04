// SPDX-License-Identifier: GPL-3.0-or-later

#define _GNU_SOURCE

#include <dirent.h>
#include <errno.h>
#include <fcntl.h>
#include <limits.h>
#include <poll.h>
#include <signal.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/prctl.h>
#include <sys/ioctl.h>
#include <sys/signalfd.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <termios.h>
#include <time.h>
#include <unistd.h>

#ifndef SYS_pidfd_open
#define SYS_pidfd_open __NR_pidfd_open
#endif

#ifndef SYS_pidfd_send_signal
#define SYS_pidfd_send_signal __NR_pidfd_send_signal
#endif

#define SUPERVISOR_FAILURE 125
#define PROC_STAT_CAPACITY 4096
#define FINAL_REAP_ATTEMPTS 200

struct process_identity {
  pid_t pid;
  pid_t process_group;
  pid_t session;
  unsigned long long start_time;
};

static bool parse_decimal_pid(const char *text, pid_t *pid) {
  char *end = NULL;
  long value;

  if (text == NULL || *text == '\0') {
    return false;
  }
  errno = 0;
  value = strtol(text, &end, 10);
  if (errno != 0 || end == text || *end != '\0' || value <= 1 ||
      value > INT_MAX) {
    return false;
  }
  *pid = (pid_t)value;
  return true;
}

static bool parse_signed_token(const char *token, long *value) {
  char *end = NULL;

  errno = 0;
  *value = strtol(token, &end, 10);
  return errno == 0 && end != token && *end == '\0';
}

static bool parse_unsigned_token(const char *token,
                                 unsigned long long *value) {
  char *end = NULL;

  errno = 0;
  *value = strtoull(token, &end, 10);
  return errno == 0 && end != token && *end == '\0';
}

static int read_process_identity(pid_t pid, struct process_identity *identity) {
  char path[64];
  char buffer[PROC_STAT_CAPACITY];
  char *cursor;
  char *save = NULL;
  char *token;
  ssize_t length;
  int descriptor;
  int field = 0;
  long process_group = 0;
  long session = 0;
  unsigned long long start_time = 0;

  if (snprintf(path, sizeof(path), "/proc/%ld/stat", (long)pid) >=
      (int)sizeof(path)) {
    return false;
  }
  descriptor = open(path, O_RDONLY | O_CLOEXEC | O_NOFOLLOW);
  if (descriptor < 0) {
    return errno == ENOENT || errno == ESRCH ? 0 : -1;
  }
  length = read(descriptor, buffer, sizeof(buffer) - 1);
  {
    int saved_error = errno;
    close(descriptor);
    errno = saved_error;
  }
  if (length < 0) {
    return errno == ENOENT || errno == ESRCH ? 0 : -1;
  }
  if (length == 0 || length >= (ssize_t)(sizeof(buffer) - 1)) {
    errno = EPROTO;
    return -1;
  }
  buffer[length] = '\0';
  cursor = strrchr(buffer, ')');
  if (cursor == NULL || cursor[1] != ' ') {
    errno = EPROTO;
    return -1;
  }
  cursor += 2;
  for (token = strtok_r(cursor, " ", &save); token != NULL;
       token = strtok_r(NULL, " ", &save), field++) {
    if (field == 2 && !parse_signed_token(token, &process_group)) {
      errno = EPROTO;
      return -1;
    }
    if (field == 3 && !parse_signed_token(token, &session)) {
      errno = EPROTO;
      return -1;
    }
    if (field == 19) {
      if (!parse_unsigned_token(token, &start_time)) {
        errno = EPROTO;
        return -1;
      }
      break;
    }
  }
  if (field != 19 || process_group < 0 || process_group > INT_MAX ||
      session < 0 || session > INT_MAX || start_time == 0) {
    errno = EPROTO;
    return -1;
  }
  identity->pid = pid;
  identity->process_group = (pid_t)process_group;
  identity->session = (pid_t)session;
  identity->start_time = start_time;
  return 1;
}

static bool same_stable_process(const struct process_identity *left,
                                const struct process_identity *right) {
  return left->pid == right->pid && left->start_time == right->start_time;
}

static int pidfd_open_process(pid_t pid) {
  return (int)syscall(SYS_pidfd_open, pid, 0U);
}

static int pidfd_signal(int pidfd, int signal_number) {
  return (int)syscall(SYS_pidfd_send_signal, pidfd, signal_number, NULL, 0U);
}

static int visit_session_members(pid_t session, pid_t supervisor,
                                 int signal_number, size_t *member_count,
                                 bool *signal_failed) {
  struct dirent *entry;
  DIR *directory;
  int saved_error = 0;

  *member_count = 0;
  directory = opendir("/proc");
  if (directory == NULL) {
    return -1;
  }
  for (;;) {
    struct process_identity before;
    struct process_identity after;
    int before_result;
    int after_result;
    int pidfd;
    pid_t pid;

    errno = 0;
    entry = readdir(directory);
    if (entry == NULL) {
      if (errno != 0) {
        saved_error = errno;
      }
      break;
    }
    if (!parse_decimal_pid(entry->d_name, &pid) || pid == supervisor) {
      continue;
    }
    before_result = read_process_identity(pid, &before);
    if (before_result < 0) {
      if (errno == EACCES || errno == EPERM) {
        continue;
      }
      saved_error = errno;
      break;
    }
    if (before_result == 0 || before.session != session) {
      continue;
    }
    pidfd = pidfd_open_process(pid);
    if (pidfd < 0) {
      if (errno == ESRCH) {
        continue;
      }
      saved_error = errno;
      break;
    }
    after_result = read_process_identity(pid, &after);
    if (after_result < 0) {
      if (errno == EACCES || errno == EPERM) {
        *signal_failed = true;
        close(pidfd);
        continue;
      }
      saved_error = errno;
      close(pidfd);
      break;
    }
    if (after_result == 0 || !same_stable_process(&before, &after) ||
        after.session != session) {
      close(pidfd);
      continue;
    }
    (*member_count)++;
    if (signal_number != 0 && pidfd_signal(pidfd, signal_number) != 0 &&
        errno != ESRCH) {
      *signal_failed = true;
    }
    close(pidfd);
  }
  closedir(directory);
  if (saved_error != 0) {
    errno = saved_error;
    return -1;
  }
  return 0;
}

static int signal_session_members(pid_t session, pid_t supervisor,
                                  int signal_number, size_t *member_count,
                                  bool *signal_failed) {
  return visit_session_members(session, supervisor, signal_number, member_count,
                               signal_failed);
}

static int count_session_members(pid_t session, pid_t supervisor,
                                 size_t *member_count) {
  bool signal_failed = false;

  return visit_session_members(session, supervisor, 0, member_count,
                               &signal_failed);
}

static void sleep_milliseconds(long milliseconds) {
  struct timespec remaining = {
      .tv_sec = milliseconds / 1000,
      .tv_nsec = (milliseconds % 1000) * 1000000L,
  };

  while (nanosleep(&remaining, &remaining) != 0 && errno == EINTR) {
  }
}

static void reap_adopted_children(pid_t direct_child, int *direct_status,
                                  bool *direct_reaped) {
  int status;
  pid_t reaped;

  for (;;) {
    reaped = waitpid(-1, &status, WNOHANG);
    if (reaped <= 0) {
      return;
    }
    if (reaped == direct_child && !*direct_reaped) {
      *direct_status = status;
      *direct_reaped = true;
    }
  }
}

static int confirm_session_quiescent(pid_t session, pid_t supervisor,
                                     pid_t direct_child, int *direct_status,
                                     bool *direct_reaped, bool *quiescent) {
  size_t members;
  int scan;

  *quiescent = false;
  for (scan = 0; scan < 2; scan++) {
    reap_adopted_children(direct_child, direct_status, direct_reaped);
    if (count_session_members(session, supervisor, &members) != 0) {
      return -1;
    }
    if (members != 0) {
      return 0;
    }
    if (scan == 0) {
      sleep_milliseconds(10);
    }
  }
  *quiescent = true;
  return 0;
}

static int clean_session(pid_t session, pid_t supervisor, pid_t direct_child,
                         int *direct_status, bool *direct_reaped) {
  size_t members = 0;
  bool signal_failed = false;
  bool quiescent = false;
  int empty_scans = 0;
  int attempt;

  reap_adopted_children(direct_child, direct_status, direct_reaped);
  if (signal_session_members(session, supervisor, SIGHUP, &members,
                             &signal_failed) != 0) {
    return -1;
  }
  if (members == 0) {
    if (confirm_session_quiescent(session, supervisor, direct_child,
                                  direct_status, direct_reaped, &quiescent) != 0) {
      return -1;
    }
    if (quiescent) {
      if (signal_failed) {
        errno = EPERM;
        return -1;
      }
      return 0;
    }
  }
  sleep_milliseconds(200);
  reap_adopted_children(direct_child, direct_status, direct_reaped);
  if (signal_session_members(session, supervisor, SIGTERM, &members,
                             &signal_failed) != 0) {
    return -1;
  }
  if (members == 0) {
    if (confirm_session_quiescent(session, supervisor, direct_child,
                                  direct_status, direct_reaped, &quiescent) != 0) {
      return -1;
    }
    if (quiescent) {
      if (signal_failed) {
        errno = EPERM;
        return -1;
      }
      return 0;
    }
  }
  sleep_milliseconds(1000);
  reap_adopted_children(direct_child, direct_status, direct_reaped);
  if (signal_session_members(session, supervisor, SIGKILL, &members,
                             &signal_failed) != 0) {
    return -1;
  }
  for (attempt = 0; attempt < FINAL_REAP_ATTEMPTS; attempt++) {
    reap_adopted_children(direct_child, direct_status, direct_reaped);
    if (count_session_members(session, supervisor, &members) != 0) {
      return -1;
    }
    if (members == 0) {
      empty_scans++;
      if (empty_scans >= 2) {
        if (signal_failed) {
          errno = EPERM;
          return -1;
        }
        return 0;
      }
    } else {
      empty_scans = 0;
    }
    if (attempt != 0 && attempt % 20 == 0 &&
        signal_session_members(session, supervisor, SIGKILL, &members,
                               &signal_failed) != 0) {
      return -1;
    }
    sleep_milliseconds(10);
  }
  errno = EBUSY;
  return -1;
}

static void raise_with_default_action(int signal_number) {
  struct sigaction action = {.sa_handler = SIG_DFL};
  sigset_t unblocked;

  sigemptyset(&action.sa_mask);
  sigaction(signal_number, &action, NULL);
  sigemptyset(&unblocked);
  sigaddset(&unblocked, signal_number);
  sigprocmask(SIG_UNBLOCK, &unblocked, NULL);
  raise(signal_number);
  _exit(128 + signal_number);
}

static void finish_with_wait_status(int status) {
  if (WIFEXITED(status)) {
    _exit(WEXITSTATUS(status));
  }
  if (WIFSIGNALED(status)) {
    raise_with_default_action(WTERMSIG(status));
  }
  _exit(SUPERVISOR_FAILURE);
}

static int ignore_supervisor_job_control_signals(void) {
  struct sigaction action = {.sa_handler = SIG_IGN};

  sigemptyset(&action.sa_mask);
  if (sigaction(SIGTSTP, &action, NULL) != 0 ||
      sigaction(SIGTTIN, &action, NULL) != 0 ||
      sigaction(SIGTTOU, &action, NULL) != 0) {
    return -1;
  }
  return 0;
}

static int reset_child_job_control_signals(void) {
  struct sigaction action = {.sa_handler = SIG_DFL};

  sigemptyset(&action.sa_mask);
  if (sigaction(SIGTSTP, &action, NULL) != 0 ||
      sigaction(SIGTTIN, &action, NULL) != 0 ||
      sigaction(SIGTTOU, &action, NULL) != 0) {
    return -1;
  }
  return 0;
}

static int wait_for_event(int signal_fd, int child_pidfd, int *close_signal) {
  struct pollfd descriptors[2] = {
      {.fd = signal_fd, .events = POLLIN},
      {.fd = child_pidfd, .events = POLLIN},
  };

  for (;;) {
    int ready = poll(descriptors, 2, -1);
    if (ready < 0) {
      if (errno == EINTR) {
        continue;
      }
      return -1;
    }
    if ((descriptors[0].revents & POLLIN) != 0) {
      struct signalfd_siginfo signal_info;
      ssize_t length = read(signal_fd, &signal_info, sizeof(signal_info));
      if (length != (ssize_t)sizeof(signal_info)) {
        return -1;
      }
      *close_signal = (int)signal_info.ssi_signo;
      return 1;
    }
    if ((descriptors[1].revents & (POLLIN | POLLHUP | POLLERR)) != 0) {
      return 0;
    }
  }
}

static int establish_controlling_terminal(pid_t supervisor) {
  pid_t terminal_session;
  pid_t terminal_group;

  if (!isatty(STDIN_FILENO)) {
    errno = ENOTTY;
    return -1;
  }
  terminal_session = tcgetsid(STDIN_FILENO);
  if (terminal_session != supervisor) {
    if (ioctl(STDIN_FILENO, TIOCSCTTY, 0) != 0) {
      return -1;
    }
    terminal_session = tcgetsid(STDIN_FILENO);
  }
  terminal_group = tcgetpgrp(STDIN_FILENO);
  if (terminal_session != supervisor || terminal_group != supervisor) {
    errno = EPERM;
    return -1;
  }
  return 0;
}

int main(int argc, char **argv) {
  struct process_identity supervisor_identity;
  sigset_t blocked;
  pid_t supervisor = getpid();
  pid_t child;
  int authorization_pipe[2];
  int child_pidfd;
  int child_status = 0;
  int close_signal = 0;
  int signal_fd;
  int event;
  bool child_reaped = false;

  if (argc < 2 || read_process_identity(supervisor, &supervisor_identity) != 1 ||
      supervisor_identity.session != supervisor ||
      supervisor_identity.process_group != supervisor || getsid(0) != supervisor ||
      getpgrp() != supervisor) {
    fprintf(stderr, "core-terminal-host-supervisor: private session invariant failed\n");
    return SUPERVISOR_FAILURE;
  }
  sigemptyset(&blocked);
  sigaddset(&blocked, SIGHUP);
  sigaddset(&blocked, SIGINT);
  sigaddset(&blocked, SIGQUIT);
  sigaddset(&blocked, SIGTERM);
  sigaddset(&blocked, SIGUSR1);
  sigaddset(&blocked, SIGUSR2);
  if (sigprocmask(SIG_BLOCK, &blocked, NULL) != 0 ||
      ignore_supervisor_job_control_signals() != 0 ||
      prctl(PR_SET_CHILD_SUBREAPER, 1, 0, 0, 0) != 0) {
    perror("core-terminal-host-supervisor: initialization");
    return SUPERVISOR_FAILURE;
  }
  if (establish_controlling_terminal(supervisor) != 0) {
    perror("core-terminal-host-supervisor: controlling terminal");
    return SUPERVISOR_FAILURE;
  }

  close(3);
  signal_fd = signalfd(-1, &blocked, SFD_CLOEXEC);
  if (signal_fd < 0) {
    perror("core-terminal-host-supervisor: signalfd");
    return SUPERVISOR_FAILURE;
  }
  if (pipe2(authorization_pipe, O_CLOEXEC) != 0) {
    perror("core-terminal-host-supervisor: pipe2");
    close(signal_fd);
    return SUPERVISOR_FAILURE;
  }

  child = fork();
  if (child < 0) {
    perror("core-terminal-host-supervisor: fork");
    close(authorization_pipe[0]);
    close(authorization_pipe[1]);
    close(signal_fd);
    return SUPERVISOR_FAILURE;
  }
  if (child == 0) {
    unsigned char authorization;
    ssize_t length;

    close(authorization_pipe[1]);
    close(signal_fd);
    if (setpgid(0, 0) != 0 || prctl(PR_SET_PDEATHSIG, SIGKILL, 0, 0, 0) != 0 ||
        getppid() != supervisor) {
      _exit(SUPERVISOR_FAILURE);
    }
    do {
      length = read(authorization_pipe[0], &authorization, sizeof(authorization));
    } while (length < 0 && errno == EINTR);
    close(authorization_pipe[0]);
    if (length != (ssize_t)sizeof(authorization) || authorization != 1 ||
        reset_child_job_control_signals() != 0 ||
        sigprocmask(SIG_UNBLOCK, &blocked, NULL) != 0) {
      _exit(SUPERVISOR_FAILURE);
    }
    execvp(argv[1], &argv[1]);
    _exit(errno == ENOENT ? 127 : 126);
  }

  close(authorization_pipe[0]);
  child_pidfd = pidfd_open_process(child);
  if (child_pidfd < 0) {
    if (errno == ENOSYS) {
      fprintf(stderr,
              "core-terminal-host-supervisor: Linux 5.3 or newer is required\n");
    } else {
      perror("core-terminal-host-supervisor: pidfd_open");
    }
    close(authorization_pipe[1]);
    while (waitpid(child, &child_status, 0) < 0 && errno == EINTR) {
    }
    close(signal_fd);
    return SUPERVISOR_FAILURE;
  }
  if (setpgid(child, child) != 0 || tcsetpgrp(STDIN_FILENO, child) != 0) {
    perror("core-terminal-host-supervisor: foreground handoff");
    close(authorization_pipe[1]);
    (void)pidfd_signal(child_pidfd, SIGKILL);
    while (waitpid(child, &child_status, 0) < 0 && errno == EINTR) {
    }
    close(child_pidfd);
    close(signal_fd);
    return SUPERVISOR_FAILURE;
  }
  {
    const unsigned char authorization = 1;
    ssize_t length;

    do {
      length = write(authorization_pipe[1], &authorization,
                     sizeof(authorization));
    } while (length < 0 && errno == EINTR);
    close(authorization_pipe[1]);
    if (length != (ssize_t)sizeof(authorization)) {
      (void)pidfd_signal(child_pidfd, SIGKILL);
      while (waitpid(child, &child_status, 0) < 0 && errno == EINTR) {
      }
      close(child_pidfd);
      close(signal_fd);
      return SUPERVISOR_FAILURE;
    }
  }
  event = wait_for_event(signal_fd, child_pidfd, &close_signal);
  close(signal_fd);
  close(child_pidfd);
  if (event < 0) {
    perror("core-terminal-host-supervisor: poll");
    close_signal = SIGTERM;
  }
  if (event == 0) {
    while (waitpid(child, &child_status, 0) < 0) {
      if (errno != EINTR) {
        perror("core-terminal-host-supervisor: waitpid");
        return SUPERVISOR_FAILURE;
      }
    }
    child_reaped = true;
  }

  if (clean_session(supervisor_identity.session, supervisor, child, &child_status,
                    &child_reaped) != 0) {
    perror("core-terminal-host-supervisor: session cleanup");
    return SUPERVISOR_FAILURE;
  }
  if (event == 0) {
    finish_with_wait_status(child_status);
  }

  raise_with_default_action(close_signal);
}
