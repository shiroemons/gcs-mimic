#!/bin/sh
# Stand-in C compiler and archiver for scripts/check-cross.sh. It creates each
# requested object or archive empty; type checking never links them.
previous=
for argument in "$@"; do
  case $argument in
    -Fo?*) : >"${argument#-Fo}" ;;
    -out:*) : >"${argument#-out:}" ;;
    -o?*) : >"${argument#-o}" ;;
    *.a | *.lib) : >"$argument" ;;
  esac
  [ "$previous" = -o ] && : >"$argument"
  previous=$argument
done
exit 0
