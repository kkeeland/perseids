#!/usr/bin/env ruby
# perseids runner: reads a GitHub Actions workflow and either plans its jobs (on your machine) or runs one job's
# steps (on the box, inside the checkout). Ruby 2.6+, stdlib only.
#
#   ruby runner.rb plan   <workflow.yml> [job...]              # TSV: wave job index state needs label note ("-" if empty)
#   ruby runner.rb decide <workflow.yml> <job> <index> <work>  # once its needs finish: "run", or "skip|unsupported <why>"
#   ruby runner.rb run    <workflow.yml> <job> [index]         # run one job (one matrix entry) in the current directory
#
# run exits 0 passed, 1 failed, 2 incomplete (a step it can't run here was skipped: SKIPPED in the summary).
# The last line is "PERSEIDS_RESULT <pass|fail|incomplete>\t<failed step>" for the CLI; the line before it,
# "PERSEIDS_OUTPUTS <json>", carries the job's outputs: to the jobs that need it.
require "yaml"
require "json"
require "tmpdir"
require "shellwords"
require "strscan"

module Perseids
  class Unsupported < StandardError; end

  # The ${{ }} subset CI files actually use: contexts, literals, == != < > && || !, and the common functions.
  class Expr
    attr_reader :reads # the contexts the value depended on: a short-circuited side doesn't count

    # success() and failure() aren't opposites for a job: when a job it needs was skipped, both are false.
    def initialize(ctx, failed = false, success = !failed)
      @ctx = ctx
      @failed = failed
      @success = success
      @reads = []
    end

    def eval(src)
      @t = tokenize(src)
      @i = 0
      v = parse_or
      raise Unsupported, "can't read ${{ #{src.strip} }}" if @i < @t.size
      v
    end

    def self.truthy(v)
      !(v.nil? || v == false || v == "" || (v.is_a?(Numeric) && (v.zero? || (v.is_a?(Float) && v.nan?))))
    end

    def self.str(v)
      case v
      when nil then ""
      when Float then v == v.floor ? v.to_i.to_s : v.to_s
      when Hash, Array then JSON.pretty_generate(v)
      else v.to_s
      end
    end

    private

    def tokenize(src)
      s = StringScanner.new(src)
      out = []
      until s.eos?
        next if s.scan(/\s+/)
        if (m = s.scan(/'(?:[^']|'')*'/)) then out << [:lit, m[1..-2].gsub("''", "'")]
        elsif (m = s.scan(/-?\d+(?:\.\d+)?(?![A-Za-z_])/)) then out << [:lit, m.include?(".") ? m.to_f : m.to_i]
        elsif (m = s.scan(/==|!=|&&|\|\||<=|>=|[<>!()\[\],.]/)) then out << [:op, m]
        elsif (m = s.scan(/[A-Za-z_][A-Za-z0-9_-]*/)) then out << [:id, m]
        else raise Unsupported, "can't read ${{ #{src.strip} }}"
        end
      end
      out
    end

    def peek(v = nil)
      t = @t[@i]
      v ? t && t[0] == :op && t[1] == v : t
    end

    def take(v = nil)
      raise Unsupported, "expected #{v}" if v && !peek(v)
      @i += 1
      @t[@i - 1] || raise(Unsupported, "unexpected end")
    end

    def parse_or
      v = parse_and
      while peek("||")
        take
        r = quietly(Expr.truthy(v)) { parse_and }
        v = Expr.truthy(v) ? v : r
      end
      v
    end

    def parse_and
      v = parse_cmp
      while peek("&&")
        take
        r = quietly(!Expr.truthy(v)) { parse_cmp }
        v = Expr.truthy(v) ? r : v
      end
      v
    end

    # Parses a side whose value won't be used without counting what it reads.
    def quietly(unused)
      was = @quiet
      @quiet ||= unused
      yield
    ensure
      @quiet = was
    end

    def parse_cmp
      v = parse_unary
      %w[== != <= >= < >].each do |op|
        next unless peek(op)
        take
        r = parse_unary
        return case op
               when "==" then eq(v, r)
               when "!=" then !eq(v, r)
               else num(v).send(op, num(r))
               end
      end
      v
    end

    def parse_unary
      return !Expr.truthy((take; parse_unary)) if peek("!")
      v = parse_primary
      loop do
        if peek(".")
          take
          v = index(v, take[1])
        elsif peek("[")
          take
          k = parse_or
          take("]")
          v = index(v, k)
        else
          return v
        end
      end
    end

    def parse_primary
      type, val = take
      return val if type == :lit
      if type == :op && val == "("
        v = parse_or
        take(")")
        return v
      end
      raise Unsupported, "unexpected #{val}" unless type == :id
      return call(val) if peek("(")
      case val
      when "true" then true
      when "false" then false
      when "null" then nil
      else
        @reads << val.downcase unless @quiet
        index(@ctx, val)
      end
    end

    def call(name)
      take("(")
      args = []
      until peek(")")
        args << parse_or
        take(",") unless peek(")")
      end
      take(")")
      case name.downcase
      when "always" then true
      when "success" then @success
      when "failure" then @failed
      when "cancelled" then false
      when "contains"
        a, b = args
        a.is_a?(Array) ? a.any? { |x| eq(x, b) } : Expr.str(a).downcase.include?(Expr.str(b).downcase)
      when "startswith" then Expr.str(args[0]).downcase.start_with?(Expr.str(args[1]).downcase)
      when "endswith" then Expr.str(args[0]).downcase.end_with?(Expr.str(args[1]).downcase)
      when "format" then Expr.str(args[0]).gsub(/\{(\d+)\}/) { Expr.str(args[$1.to_i + 1]) }
      when "join" then Array(args[0]).map { |x| Expr.str(x) }.join(args.size > 1 ? Expr.str(args[1]) : ",")
      when "tojson" then JSON.pretty_generate(args[0])
      when "fromjson" then JSON.parse(Expr.str(args[0]))
      when "hashfiles" then "" # only cache keys use it, and caches are skipped
      else raise Unsupported, "#{name}() isn't supported"
      end
    end

    def index(v, k)
      case v
      when Hash
        return v[k] if v.key?(k)
        pair = v.find { |kk, _| kk.to_s.casecmp?(k.to_s) }
        pair && pair[1]
      when Array then k.is_a?(Numeric) ? v[k.to_i] : nil
      end
    end

    def eq(a, b)
      return a.casecmp?(b) if a.is_a?(String) && b.is_a?(String)
      return a == b if a.class == b.class || (a.is_a?(Numeric) && b.is_a?(Numeric))
      num(a) == num(b)
    end

    def num(v)
      case v
      when nil, false then 0
      when true then 1
      when Numeric then v
      else v.strip.empty? ? 0 : (Float(v) rescue Float::NAN)
      end
    end
  end

  module_function

  def interpolate(v, ctx, failed = false)
    return v unless v.is_a?(String)
    v.gsub(/\$\{\{(.*?)\}\}/m) { Expr.str(Expr.new(ctx, failed).eval(Regexp.last_match(1))) }
  end

  STATUS = /\b(always|success|failure|cancelled)\s*\(/i.freeze

  # An `if:` with no status function means success() && (it), as on GitHub. Boxes get no secrets and perseids has no
  # vars, so an `if:` whose value depends on either can't be decided here: it's unsupported, never a quiet skip.
  def cond(src, ctx, failed, success = !failed)
    return success if src.nil?
    return src && success if src == true || src == false
    s = src.to_s.strip
    s = s[3..-3] if s.start_with?("${{") && s.end_with?("}}")
    e = Expr.new(ctx, failed, success)
    v = Expr.truthy(e.eval(s))
    unknown = e.reads & %w[secrets vars]
    raise Unsupported, "its if: reads #{unknown.join(" and ")}, which perseids doesn't have" unless unknown.empty?
    s =~ STATUS ? v : v && success
  end

  # Whether one entry of a job runs, once the jobs it needs have finished: ["run" | "skip" | "unsupported", why].
  # needs is the needs context: {"job" => {"result" => "success|failure|skipped", "outputs" => {...}}}.
  def decide(job, ctx, needs = {})
    ok = needs.values.all? { |n| n["result"] == "success" }
    failed = needs.values.any? { |n| n["result"] == "failure" }
    ctx = ctx.merge("needs" => needs)
    unless cond(job["if"], ctx, failed, ok)
      return ["skip", "its if: is false"] if ok || job["if"].to_s =~ STATUS
      return ["skip", "needs #{needs.reject { |_, n| n["result"] == "success" }.keys.join(" ")}"]
    end
    runs_on = Array(job["runs-on"].is_a?(Hash) ? job["runs-on"]["labels"] : job["runs-on"]).map { |l| Expr.str(interpolate(l, ctx)) }
    return ["unsupported", "it calls a reusable workflow"] if job["uses"]
    return ["unsupported", "it runs in a container"] if job["container"]
    os = runs_on.find { |l| l =~ /macos|windows/i }
    os ? ["unsupported", "it runs on #{os}"] : ["run", ""]
  rescue Unsupported => e
    ["unsupported", e.message]
  rescue StandardError => e # fromJSON('') and the like: it can't be decided, which is never a pass
    ["unsupported", "its if: failed: #{e.message}"]
  end

  # The needs context, from the CLI's work folder: <job>-<i>/state starts with that entry's result
  # (pass|fail|incomplete|error|skip|unsupported) and <job>-<i>/outputs holds its job outputs as JSON.
  # As on GitHub, a matrix job failed if any entry did, and was skipped if none ran.
  def needs_ctx(job, work)
    Array(job["needs"]).map do |n|
      dirs = Dir.children(work).select { |d| d =~ /\A#{Regexp.escape(n)}-\d+\z/ }.sort_by { |d| d[/\d+\z/].to_i }
      states = dirs.map { |d| (File.read(File.join(work, d, "state")).split.first rescue "").to_s }
      result = if states.any? { |s| %w[fail error].include?(s) } then "failure"
               elsif states.any? { |s| %w[pass incomplete].include?(s) } then "success"
               else "skipped"
               end
      outputs = dirs.map { |d| (JSON.parse(File.read(File.join(work, d, "outputs"))) rescue {}) }.reduce({}, :merge)
      [n, { "result" => result, "outputs" => outputs }]
    end.to_h
  end

  def load(path)
    YAML.safe_load(File.read(path), aliases: true)
  end

  def matrix(job)
    m = (job["strategy"] || {})["matrix"]
    return [{}] if m.nil?
    raise Unsupported, "its matrix is built by an expression" if m.is_a?(String)
    inc = m["include"] || []
    exc = m["exclude"] || []
    keys = m.keys - %w[include exclude]
    combos = keys.inject([{}]) do |acc, k|
      raise Unsupported, "matrix.#{k} is an expression" unless m[k].is_a?(Array)
      acc.flat_map { |c| m[k].map { |x| c.merge(k => x) } }
    end
    combos = [] if keys.empty?
    combos.reject! { |c| exc.any? { |e| e.all? { |k, v| c[k] == v } } }
    orig = combos.map(&:dup)
    extra = []
    # An include entry joins every combination whose original values it doesn't overwrite, else it is its own.
    inc.each do |e|
      hit = false
      combos.each_with_index do |c, i|
        next unless e.all? { |k, v| !orig[i].key?(k) || orig[i][k] == v }
        c.merge!(e)
        hit = true
      end
      extra << e.dup unless hit
    end
    combos + extra
  end

  def github_ctx(job_id = "")
    ref = ENV["PERSEIDS_REF_NAME"].to_s
    ref = `git rev-parse --abbrev-ref HEAD 2>/dev/null`.strip if ref.empty?
    { "sha" => ENV["PERSEIDS_SHA"] || `git rev-parse HEAD 2>/dev/null`.strip, "ref" => "refs/heads/#{ref}",
      "ref_name" => ref, "head_ref" => "", "base_ref" => "", "event_name" => ENV.fetch("PERSEIDS_EVENT", "push"),
      "repository" => ENV["PERSEIDS_REPOSITORY"].to_s, "repository_owner" => ENV["PERSEIDS_REPOSITORY"].to_s.split("/").first.to_s,
      "workspace" => Dir.pwd, "job" => job_id, "run_id" => Process.pid.to_s, "run_number" => "1", "run_attempt" => "1",
      "actor" => "perseids", "server_url" => "https://github.com", "event" => {} }
  end

  def base_ctx(wf, job_id = "")
    needs = (JSON.parse(ENV["PERSEIDS_NEEDS"].to_s) rescue {}) # on a box, from the CLI's decide
    ctx = { "github" => github_ctx(job_id), "env" => {}, "matrix" => {}, "secrets" => {}, "vars" => {}, "inputs" => {},
            "needs" => needs, "steps" => {}, "job" => { "services" => {} }, "strategy" => {},
            "runner" => { "os" => "Linux", "arch" => "X64", "name" => "perseids", "temp" => Dir.tmpdir } }
    (wf["env"] || {}).each { |k, v| ctx["env"][k] = Expr.str(interpolate(v, ctx)) }
    ctx
  end

  # Waves follow `needs:`, and asked-for jobs bring the jobs they need. A job with no needs is decided now; one with
  # needs is a "gate": the CLI decides it (`decide`) when they finish, with their real results and outputs, since an
  # `if:` read before then sees an empty needs context.
  def plan(wf, only)
    jobs = wf["jobs"] || {}
    (only - jobs.keys).each { |j| raise Unsupported, "no job #{j} (jobs: #{jobs.keys.join(", ")})" }
    wave = {}
    depth = lambda do |id, seen|
      raise Unsupported, "needs: loop at #{id}" if seen.include?(id)
      raise Unsupported, "a job needs #{id}, which isn't in the workflow" unless jobs[id]
      wave[id] ||= Array(jobs[id]["needs"]).map { |n| depth.call(n, seen + [id]) + 1 }.max || 0
    end
    (only.empty? ? jobs.keys : only).each { |id| depth.call(id, []) } # wave ends up keyed by every job that runs
    ctx = base_ctx(wf)
    rows = []
    wave.keys.sort_by { |id| [wave[id], jobs.keys.index(id)] }.each do |id|
      job = jobs[id]
      needs = Array(job["needs"]).join(",")
      combos = begin
        matrix(job)
      rescue Unsupported => e
        rows << [wave[id], id, 0, "unsupported", needs, id, e.message]
        next
      end
      combos.each_with_index do |combo, i|
        label = combo.empty? ? id : "#{id} (#{combo.values.map { |v| Expr.str(v) }.join(", ")})"
        state, note = needs.empty? ? decide(job, ctx.merge("matrix" => combo)) : ["gate", ""]
        rows << [wave[id], id, i, state, needs, label, note]
      end
    end
    rows
  end

  # --- run: one job's steps, the way a GitHub-hosted runner would ---

  NOOP = %w[actions/checkout actions/cache actions/cache/restore actions/cache/save actions/upload-artifact
            actions/upload-pages-artifact docker/setup-buildx-action docker/setup-qemu-action].freeze
  SETUP = { # action => [mise tool, version input, version-file input, binary]
    "actions/setup-node" => ["node", "node-version", "node-version-file", "node"],
    "actions/setup-go" => ["go", "go-version", "go-version-file", "go"],
    "actions/setup-python" => ["python", "python-version", "python-version-file", "python3"],
    "actions/setup-java" => ["java", "java-version", "java-version-file", "java"],
    "ruby/setup-ruby" => ["ruby", "ruby-version", nil, "ruby"],
    "pnpm/action-setup" => ["pnpm", "version", nil, "pnpm"],
    "oven-sh/setup-bun" => ["bun", "bun-version", "bun-version-file", "bun"],
    "denoland/setup-deno" => ["deno", "deno-version", nil, "deno"],
    "astral-sh/setup-uv" => ["uv", "version", nil, "uv"],
    "dtolnay/rust-toolchain" => ["rust", "toolchain", nil, "cargo"],
  }.freeze

  class Job
    def initialize(wf, job_id, index)
      @wf = wf
      @id = job_id
      @job = wf["jobs"].fetch(job_id)
      @tmp = Dir.mktmpdir("perseids")
      @ctx = Perseids.base_ctx(wf, job_id)
      @ctx["matrix"] = Perseids.matrix(@job)[index] || raise(Unsupported, "no matrix entry #{index}")
      @ctx["runner"]["temp"] = @tmp
      @env = ENV.to_h.merge(
        "CI" => "true", "GITHUB_ACTIONS" => "true", "GITHUB_SHA" => @ctx["github"]["sha"], "GITHUB_REF" => @ctx["github"]["ref"],
        "GITHUB_REF_NAME" => @ctx["github"]["ref_name"], "GITHUB_WORKSPACE" => Dir.pwd, "GITHUB_JOB" => job_id,
        "GITHUB_REPOSITORY" => @ctx["github"]["repository"], "GITHUB_EVENT_NAME" => @ctx["github"]["event_name"],
        "RUNNER_OS" => "Linux", "RUNNER_ARCH" => "X64", "RUNNER_TEMP" => @tmp
      ).merge(@ctx["env"])
      (@job["env"] || {}).each { |k, v| @ctx["env"][k] = @env[k] = Expr.str(Perseids.interpolate(v, @ctx)) }
      @defaults = ((@wf["defaults"] || {})["run"] || {}).merge((@job["defaults"] || {})["run"] || {})
      @keep_going = ENV["PERSEIDS_KEEP_GOING"] == "1"
      @results = []
      @failed = false
    end

    def run
      start_services
      (@job["steps"] || []).each_with_index { |step, i| run_step(step, i) }
      summary
    end

    private

    def log(msg)
      puts msg
      $stdout.flush
    end

    def docker
      @docker ||= system("docker info >/dev/null 2>&1") ? ["docker"] : %w[sudo -n docker]
    end

    def start_services
      (@job["services"] || {}).each do |name, svc|
        image = Expr.str(Perseids.interpolate(svc["image"], @ctx))
        next if image.empty? # an empty image turns the service off, as on GitHub
        t = Time.now
        log "\n=== service #{name} (#{image})"
        cname = "perseids-#{name}-#{Process.pid}"
        ports = {}
        args = [*docker, "run", "-d", "--name", cname]
        (svc["env"] || {}).each { |k, v| args += ["-e", "#{k}=#{Expr.str(Perseids.interpolate(v, @ctx))}"] }
        Array(svc["ports"]).each do |p|
          p = p.to_s
          p = "#{p}:#{p}" unless p.include?(":")
          ports[p.split(":").last] = p.split(":")[-2]
          args += ["-p", p]
        end
        Array(svc["volumes"]).each { |v| args += ["-v", v.to_s] }
        args += Shellwords.split(Expr.str(Perseids.interpolate(svc["options"], @ctx))) if svc["options"]
        args << image
        ok = system(*args, out: File::NULL) && wait_healthy(cname)
        @ctx["job"]["services"][name] = { "id" => cname, "ports" => ports, "network" => "" }
        @results << ["service #{name}", ok ? "ok #{(Time.now - t).round}s" : "FAIL"]
        @failed ||= !ok
      end
    end

    def wait_healthy(cname)
      150.times do
        s = `#{Shellwords.join(docker)} inspect -f '{{if .State.Health}}{{.State.Health.Status}}{{else}}{{.State.Status}}{{end}}' #{cname} 2>/dev/null`.strip
        return true if %w[healthy running].include?(s)
        return false if %w[exited dead].include?(s)
        sleep 2
      end
      false
    end

    def run_step(step, i)
      sctx = @ctx.merge("env" => @ctx["env"].dup)
      (step["env"] || {}).each { |k, v| sctx["env"][k] = Expr.str(Perseids.interpolate(v, sctx)) }
      name = Expr.str(Perseids.interpolate(step["name"], sctx)) if step["name"]
      name ||= step["uses"] || step["run"].to_s.lines.first.to_s.strip
      name = name.empty? ? "step #{i}" : name
      begin
        unless Perseids.cond(step["if"], sctx, @failed && !@keep_going)
          @results << [name, @failed ? "skipped (an earlier step failed)" : "skipped (if)"]
          return
        end
        t = Time.now
        ok, note = step["uses"] ? uses(step, sctx, name, i) : script(step, sctx, name, i)
        soft = Expr.truthy(Perseids.interpolate(step["continue-on-error"], sctx))
        @ctx["steps"][step["id"]]["outcome"] = ok ? "success" : "failure" if step["id"] && @ctx["steps"][step["id"]]
        @results << [name, ok ? (note || "ok #{(Time.now - t).round}s") : (soft ? "failed (continue-on-error)" : "FAIL")]
        @failed ||= !ok && !soft
      rescue Unsupported => e
        log "\n=== [#{i}] #{name}\nSKIPPED: #{e.message}"
        @results << [name, "SKIPPED (#{e.message})"]
      end
    end

    def script(step, sctx, name, i)
      body = Perseids.interpolate(step["run"].to_s, sctx)
      dir = Perseids.interpolate(step["working-directory"] || @defaults["working-directory"], sctx)
      shell = step["shell"] || @defaults["shell"]
      file = File.join(@tmp, "step-#{i}")
      File.write(file, body)
      cmd = case shell
            when nil then ["bash", "-e", file] # GitHub's default; an explicit `shell: bash` adds pipefail
            when "bash" then ["bash", "--noprofile", "--norc", "-eo", "pipefail", file]
            when "sh" then ["sh", "-e", file]
            when "python" then ["python3", file]
            else shell.include?("{0}") ? Shellwords.split(shell.sub("{0}", file)) : raise(Unsupported, "shell #{shell}")
            end
      log "\n=== [#{i}] #{name}"
      files = %w[GITHUB_ENV GITHUB_PATH GITHUB_OUTPUT GITHUB_STEP_SUMMARY].map { |k| [k, File.join(@tmp, "#{k}-#{i}")] }.to_h
      files.each_value { |f| File.write(f, "") }
      env = @env.merge(sctx["env"]).merge(files)
      mins = step["timeout-minutes"] && Perseids.interpolate(step["timeout-minutes"], sctx).to_f
      ok = spawn_wait(env, cmd, dir ? File.expand_path(dir) : Dir.pwd, mins)
      read_kv(files["GITHUB_ENV"]).each { |k, v| @ctx["env"][k] = @env[k] = v }
      File.readlines(files["GITHUB_PATH"]).map(&:strip).reject(&:empty?).each { |p| @env["PATH"] = "#{p}:#{@env["PATH"]}" }
      @ctx["steps"][step["id"]] = { "outputs" => read_kv(files["GITHUB_OUTPUT"]), "outcome" => "", "conclusion" => "" } if step["id"]
      [ok, nil]
    end

    def spawn_wait(env, cmd, dir, mins)
      pid = Process.spawn(env, *cmd, chdir: dir, pgroup: true)
      deadline = mins && Time.now + mins * 60
      loop do
        _, st = Process.wait2(pid, Process::WNOHANG)
        return st.success? if st
        if deadline && Time.now > deadline
          log "timed out after #{mins} min"
          Process.kill("TERM", -pid) rescue nil
          sleep 5
          Process.kill("KILL", -pid) rescue nil
          Process.wait(pid) rescue nil
          return false
        end
        sleep 0.2
      end
    end

    def read_kv(path)
      out = {}
      lines = File.readlines(path, chomp: true)
      until lines.empty?
        l = lines.shift
        if l =~ /\A([^=<]+)<<(.+)\z/
          k, d = Regexp.last_match(1), Regexp.last_match(2)
          val = []
          while (x = lines.shift) && x != d
            val << x
          end
          out[k] = val.join("\n")
        elsif l.include?("=")
          k, v = l.split("=", 2)
          out[k] = v
        end
      end
      out
    end

    def uses(step, sctx, name, i)
      return composite(step, sctx, name, i) if step["uses"].start_with?("./")
      action, ref = step["uses"].split("@", 2)
      action = action.downcase
      return [true, "ok (nothing to do on a fresh box)"] if NOOP.include?(action)
      tool, key, file_key, bin = SETUP[action]
      raise Unsupported, "#{step["uses"]} isn't supported" unless tool
      with = (step["with"] || {}).map { |k, v| [k, Expr.str(Perseids.interpolate(v, sctx))] }.to_h
      log "\n=== [#{i}] #{name}"
      ver = with[key].to_s
      ver = ref.to_s if ver.empty? && action == "dtolnay/rust-toolchain"
      ver = version_file(with[file_key] || default_version_file(tool)) if ver.empty?
      if ver.empty? && tool == "pnpm" && File.exist?("package.json")
        ver = JSON.parse(File.read("package.json"))["packageManager"].to_s[/\Apnpm@([^+]+)/, 1].to_s
      end
      ver = ver.strip.sub(/\Av/, "").sub(/(\.x)+\z/, "").sub(%r{\Alts/.*}, "lts")
      ver = "latest" if %w[stable latest *].include?(ver)
      if ver.empty?
        return [true, "ok (the box's #{bin})"] if system("command -v #{bin} >/dev/null 2>&1")
        ver = "latest"
      end
      ver = "#{with["distribution"]}-#{ver}" if tool == "java" && %w[temurin zulu corretto].include?(with["distribution"])
      ok = mise_install("#{tool}@#{ver}")
      ok &&= spawn_wait(@env, %w[bundle install --jobs 4], with["working-directory"] || Dir.pwd, nil) if tool == "ruby" && Expr.truthy(with["bundler-cache"])
      [ok, ok ? "ok #{tool}@#{ver}" : nil]
    end

    # A local composite action (uses: ./path): its steps run here, in order, with its inputs.
    def composite(step, sctx, name, i)
      dir = step["uses"].chomp("/")
      file = %w[action.yml action.yaml].map { |f| File.join(dir, f) }.find { |f| File.exist?(f) }
      raise Unsupported, "no action.yml in #{dir}" unless file
      runs = Perseids.load(file)["runs"] || {}
      raise Unsupported, "#{dir} is a #{runs["using"]} action, not composite" unless runs["using"] == "composite"
      with = step["with"] || {}
      defs = Perseids.load(file)["inputs"] || {}
      inputs = defs.map { |k, d| [k, Expr.str(Perseids.interpolate(with.key?(k) ? with[k] : (d || {})["default"], sctx))] }.to_h
      log "\n=== [#{i}] #{name} (composite, #{(runs["steps"] || []).size} steps)"
      saved, before = @ctx["inputs"], @failed
      begin
        @ctx["inputs"], @failed = inputs, false # inside the action, success() means its own steps so far
        (runs["steps"] || []).each_with_index { |cs, j| run_step(cs, "#{i}.#{j}") }
        ok = !@failed
      ensure
        @ctx["inputs"], @failed = saved, before
      end
      [ok, ok ? "ok (composite)" : nil]
    end

    def default_version_file(tool)
      { "go" => nil, "node" => ".nvmrc", "python" => ".python-version", "ruby" => ".ruby-version" }[tool]
    end

    def version_file(path)
      return "" if path.nil? || path.empty? || !File.exist?(path)
      text = File.read(path)
      return (text[/^toolchain go(\S+)/, 1] || text[/^go (\S+)/, 1]).to_s if File.basename(path) == "go.mod"
      text.lines.map(&:strip).reject { |l| l.empty? || l.start_with?("#") }.first.to_s.split.last.to_s
    end

    def mise_install(spec)
      mise = `command -v mise 2>/dev/null`.strip
      mise = File.expand_path("~/.local/bin/mise") if mise.empty?
      unless File.executable?(mise)
        return false unless system("curl -fsSL https://mise.run | sh >/dev/null")
      end
      return false unless system(@env, mise, "install", "-y", spec)
      bins = `#{Shellwords.escape(mise)} bin-paths #{Shellwords.escape(spec)} 2>/dev/null`.split("\n").reject(&:empty?)
      bins = ["#{`#{Shellwords.escape(mise)} where #{Shellwords.escape(spec)}`.strip}/bin"] if bins.empty?
      bins.reverse_each { |b| @env["PATH"] = "#{b}:#{@env["PATH"]}" }
      log "using #{spec} from #{bins.first}"
      true
    end

    def summary
      head = @ctx["github"]["sha"]
      log "\n--- perseids: job #{@id}#{@ctx["matrix"].empty? ? "" : " #{@ctx["matrix"].to_json}"} at #{head}"
      @results.each_with_index { |(n, s), i| log format("  [%2d] %-60s %s", i, n[0, 60], s) }
      failed = @results.find { |r| r[1] == "FAIL" }
      skipped = @results.count { |r| r[1].start_with?("SKIPPED") }
      state = failed ? "fail" : skipped > 0 ? "incomplete" : "pass"
      log({ "fail" => "FAILED", "incomplete" => "INCOMPLETE: #{skipped} step(s) skipped, run them another way before calling this green",
            "pass" => "PASSED" }[state])
      outputs = (@job["outputs"] || {}).map { |k, v| [k, (Expr.str(Perseids.interpolate(v, @ctx)) rescue "")] }.to_h
      log "PERSEIDS_OUTPUTS #{outputs.to_json}"
      log "PERSEIDS_RESULT #{state}\t#{failed ? failed[0] : ""}"
      { "fail" => 1, "incomplete" => 2, "pass" => 0 }[state]
    end
  end
end

if $PROGRAM_NAME == __FILE__
  mode, wf_path, *rest = ARGV
  usage = "usage: runner.rb plan <workflow> [job...] | decide <workflow> <job> <index> <work> | run <workflow> <job> [index]"
  abort usage unless %w[plan decide run].include?(mode) && wf_path
  begin
    wf = Perseids.load(wf_path)
    if mode == "plan"
      Perseids.plan(wf, rest).each { |r| puts r.map { |c| c.to_s.empty? ? "-" : c.to_s.tr("\t\n", "  ") }.join("\t") }
    elsif mode == "decide" # also leaves <work>/<job>-<index>/needs.json, the needs context the box gets
      id, index, work = rest[0], rest[1].to_i, rest[2]
      job = wf["jobs"].fetch(id)
      needs = Perseids.needs_ctx(job, work)
      File.write(File.join(work, "#{id}-#{index}", "needs.json"), needs.to_json)
      state, note = Perseids.decide(job, Perseids.base_ctx(wf, id).merge("matrix" => Perseids.matrix(job)[index] || {}), needs)
      puts [state, note.tr("\t\n", "  ")].reject(&:empty?).join(" ")
    else
      exit Perseids::Job.new(wf, rest[0], rest[1].to_i).run
    end
  rescue Perseids::Unsupported => e
    warn "perseids: #{e.message}"
    puts "PERSEIDS_RESULT incomplete\t" if mode == "run"
    exit 2
  end
end
