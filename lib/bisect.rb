#!/usr/bin/env ruby
# perseids bisect, the local half: picks the commits to test and reads their results. Ruby 2.6+, stdlib only.
#
#   ruby bisect.rb range <good|""> <bad> <n>   # "sha<TAB>subject" lines, oldest first: good, then good..bad
#   ruby bisect.rb report <logdir> < results   # results: "sha<TAB>subject<TAB>state"; prints the timeline and verdict
#
# report exits 0 found the first bad commit, 1 none or inconsistent, 99 a box failed,
# 3 sampled and the break is between two tested commits (it prints "good bad" to narrow to).
require "open3"

module Perseids
  module Bisect
    module_function

    def git(*args)
      out, st = Open3.capture2("git", *args, err: File::NULL)
      st.success? ? out : nil
    end

    def sha(ref)
      git("rev-parse", "--verify", "-q", "#{ref}^{commit}").to_s.strip.then { |s| s.empty? ? nil : s }
    end

    # The merge-base with origin's default branch if that's behind bad, else n-1 first-parent commits back (or the root).
    def default_good(bad, n)
      %w[origin/HEAD origin/main origin/master].each do |r|
        next unless sha(r)
        base = git("merge-base", r, bad).to_s.strip
        return base unless base.empty? || base == bad
      end
      git("rev-list", "--first-parent", "--max-count=#{n}", bad).split.last
    end

    # Evenly spread, both ends kept.
    def sample(list, n)
      return list if list.size <= n
      (0...n).map { |i| list[(i * (list.size - 1).to_f / (n - 1)).round] }.uniq
    end

    # [all, tested]: [sha, subject] pairs, oldest first. The good end is tested too.
    def range(good, bad, n)
      raise "-n must be 3 or more" if n < 3
      b = sha(bad) || raise("no commit #{bad}")
      g = good.to_s.empty? ? default_good(b, n) : sha(good) || raise("no commit #{good}")
      raise "good and bad are the same commit" if g == b
      raise "#{good} isn't an ancestor of #{bad}" unless git("merge-base", "--is-ancestor", g, b)
      line = ->(l) { l.split("\t", 2).map(&:strip) }
      all = [line.call(git("log", "-1", "--format=%H%x09%s", g))] +
            git("log", "--first-parent", "--reverse", "--format=%H%x09%s", "#{g}..#{b}").lines.map(&line)
      [all, sample(all, n)]
    end

    # states: one per commit in the full range, oldest first: "pass", "fail", "error", or nil (not tested).
    def verdict(states)
      t = states.each_index.select { |i| states[i] }
      v = { errors: t.count { |i| states[i] == "error" }, good_fails: states[t.first] == "fail", bad_passes: states[t.last] == "pass" }
      v[:good], v[:bad] = t.each_cons(2).find { |a, b| states[a] == "pass" && states[b] == "fail" }
      f = t.find { |i| states[i] == "fail" }
      p = f && t.find { |i| i > f && states[i] == "pass" }
      v[:flip] = [f, p] if p
      v
    end

    def report(logdir, input)
      rows = input.lines.map { |l| l.chomp.split("\t", 3) }
      all = [rows.first[0]] + git("rev-list", "--first-parent", "--reverse", "#{rows.first[0]}..#{rows.last[0]}").split
      at = all.each_with_index.to_h
      states = Array.new(all.size)
      rows.each { |s, _, st| states[at.fetch(s)] = st.split.first }
      v = verdict(states)
      short = ->(i) { all[i][0, 7] }
      tested = rows.map { |s, _, _| at[s] }
      say = ->(m) { warn m }

      say.call "--- perseids bisect: #{rows.size} commits, oldest first"
      rows.each_with_index do |(s, subj, st), k|
        gap = k.zero? ? 0 : tested[k] - tested[k - 1] - 1
        say.call format("  %-6s %-9s(%d not tested)", "", "...", gap) if gap > 0
        mark = at[s] == v[:bad] && v[:bad] - v[:good] == 1 ? "  <- first bad" : ""
        note = st.start_with?("error") ? "  (#{st.split(" ", 2)[1]})" : ""
        say.call format("  %-6s %s  %s%s%s", st.split.first, s[0, 7], subj.to_s[0, 60], mark, note)
      end

      if v[:errors] > 0
        say.call "--- #{v[:errors]} box(es) failed (Boat or SSH, not your code): rerun"
        return 99
      end
      say.call "--- the good end #{short.call(0)} fails too: the break is older; pass an older --good" if v[:good_fails]
      say.call "--- the bad end #{short.call(-1)} passes: nothing in this range breaks the command" if v[:bad_passes]
      if v[:flip]
        f, p = v[:flip]
        say.call "--- not monotonic (pass, fail, pass): #{short.call(p)} passes after #{short.call(f)} fails; a flaky test, or a fix and a new break"
      end
      return 1 unless v[:bad] && !v[:flip] && !v[:good_fails] && !v[:bad_passes]

      if v[:bad] - v[:good] > 1
        say.call "--- the break is in the #{v[:bad] - v[:good] - 1} untested commits between #{short.call(v[:good])} and #{short.call(v[:bad])}: narrowing"
        puts "#{all[v[:good]]} #{all[v[:bad]]}"
        return 3
      end
      s = all[v[:bad]]
      say.call "--- first bad commit: #{s}"
      say.call git("log", "-1", "--format=    %s%n    %an <%ae>, %ad", "--date=format:%Y-%m-%d %H:%M", s)
      say.call "    git show --stat #{s[0, 12]}"
      log = File.join(logdir, "bisect-#{tested.index(v[:bad])}.log")
      say.call "    log: #{log}" if File.exist?(log)
      0
    end
  end
end

if $PROGRAM_NAME == __FILE__
  mode, *args = ARGV
  case mode
  when "range"
    begin
      all, tested = Perseids::Bisect.range(args[0], args[1], args[2].to_i)
    rescue RuntimeError => e
      abort "perseids: #{e.message}"
    end
    warn "perseids: #{all.size} commits in range; testing #{tested.size} spread evenly, then narrowing" if tested.size < all.size
    tested.each { |s, subj| puts "#{s}\t#{subj}" }
  when "report" then exit Perseids::Bisect.report(args[0], $stdin.read)
  else abort "usage: bisect.rb range <good> <bad> <n> | report <logdir>"
  end
end
