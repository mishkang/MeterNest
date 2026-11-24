#include <sys/xattr.h>
#include <stdio.h>
#include <stdlib.h>

int main(int argc, char **argv) {
  if (argc != 3) return 2;
  FILE *file = fopen(argv[2], "rb");
  if (!file) return 3;
  if (fseek(file, 0, SEEK_END) != 0) return 4;
  long size = ftell(file);
  if (size < 0 || fseek(file, 0, SEEK_SET) != 0) return 5;
  char *body = malloc((size_t)size);
  if (!body) return 6;
  if (fread(body, 1, (size_t)size, file) != (size_t)size) return 7;
  fclose(file);
  int result = setxattr(argv[1], "com.apple.ResourceFork", body, (size_t)size, 0, 0);
  free(body);
  return result == 0 ? 0 : 8;
}
