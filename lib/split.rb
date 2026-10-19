#!/usr/bin/env ruby
# perseids split: one command, N boxes. Runs on your machine. Ruby 2.6+, stdlib only.
#
#   ruby split.rb <sha> <n> <glob|-> <command> <outdir>
#
# Writes <outdir>/shard-<i>.cmd (the command for shard i, 1-based) and prints one plan line per shard for the CLI.
# {i} and {n} become the shard index and count. With a glob, the tracked files at <sha> that match it are split into
# n groups balanced by size, and {files} becomes the shard's group, shell-quoted (appended if {files} isn't there).
require "shellwords"
require "tmpdir"

module PerseidsSplit
  module_function

  # path => size for the tracked files at sha that match glob (git's own glob pathspec, so ** works as in git).
  def files(sha, glob)
    sizes = {}
    IO.popen(["git", "ls-tree", "-r", "-l", "-z", sha], &:read).split("\0").each do |l|
      meta, path = l.split("\t", 2)
      sizes[path] = meta.split.last.to_i
    end
    Dir.mktmpdir do |d| # an index of exactly that commit, so dirty and staged state here doesn't matter
      env = { "GIT_INDEX_FILE" => File.join(d, "index") }
      system(env, "git", "read-tree", sha) || raise("git read-tree #{sha} failed")
      names = IO.popen(env, ["git", "ls-files", "-z", "--", ":(glob)#{glob}"], &:read).split("\0")
      names.map { |p| [p, sizes.fetch(p, 0)] }.to_h
    end
  end

  # Largest first onto the lightest group; ties go to the lower path and the lower group, so it is deterministic.
  def groups(files, n)
    out = Array.new(n) { [] }
    weight = Array.new(n, 0)
    files.sort_by { |p, s| [-s, p] }.each do |p, s|
      g = weight.each_with_index.min[1]
      out[g] << p
      weight[g] += s
    end
    out.map(&:sort)
  end

  def command(cmd, i, n, files = nil)
    c = cmd.gsub("{i}", i.to_s).gsub("{n}", n.to_s)
    return c if files.nil?
    q = files.map { |f| Shellwords.escape(f) }.join(" ")
    c.include?("{files}") ? c.gsub("{files}") { q } : "#{c} #{q}" # a block: a string would read \' in a name as a backreference
  end
end

if $PROGRAM_NAME == __FILE__
  sha, n, glob, cmd, dir = ARGV
  abort "usage: split.rb <sha> <n> <glob|-> <command> <outdir>" unless dir && n.to_i > 0
  n = n.to_i
  if glob == "-"
    abort "perseids: {files} needs --files '<glob>'" if cmd.include?("{files}")
    shards = (1..n).map { |i| [PerseidsSplit.command(cmd, i, n), "shard #{i}/#{n}"] }
  else
    f = PerseidsSplit.files(sha, glob)
    abort "perseids: no tracked files match #{glob}" if f.empty?
    n = [n, f.size].min # never a box with nothing to run
    shards = PerseidsSplit.groups(f, n).each_with_index.map do |g, k|
      [PerseidsSplit.command(cmd, k + 1, n, g), "shard #{k + 1}/#{n} (#{g.size} file#{g.size == 1 ? "" : "s"})"]
    end
  end
  shards.each_with_index do |(c, label), k|
    File.write(File.join(dir, "shard-#{k + 1}.cmd"), c)
    puts ["0", "exec", k + 1, "run", "-", label, "-"].join("\t")
  end
end
